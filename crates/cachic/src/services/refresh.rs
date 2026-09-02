//! Automatic refresh of the `cache-domains` list (FR-61).
//!
//! CDNs move, and `uklans/cache-domains` tracks them. An operator should not have to redeploy to
//! keep caching Steam.
//!
//! Two properties make this safe enough to run unattended:
//!
//! - **Validate before applying.** A malformed or empty refresh is rejected and the previous list
//!   keeps serving. An automatic update path that can break the cache is worse than manual
//!   updates, because it breaks it at 3am.
//! - **Swap, don't rebuild.** The live matcher is behind an `ArcSwap`, so applying a new list is a
//!   pointer store. A reload must not appear in p99.

use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use arc_swap::ArcSwap;

use super::{domains::DomainList, matcher::Matcher};

#[derive(Debug, Default)]
pub struct RefreshStats {
    pub attempts: AtomicU64,
    pub applied: AtomicU64,
    /// Fetched successfully but identical to what we already had.
    pub unchanged: AtomicU64,
    /// Fetched but rejected by validation. The previous list is still serving.
    pub rejected: AtomicU64,
    pub failed: AtomicU64,
}

/// The live domain list and matcher.
///
/// Readers take the matcher through `load()`, which is a pointer read. Writers publish a whole
/// new matcher; nothing is mutated in place, so a reload cannot be observed half-applied.
pub struct LiveServices {
    matcher: ArcSwap<Matcher>,
    list: ArcSwap<DomainList>,
    stats: Arc<RefreshStats>,
}

impl LiveServices {
    pub fn new(list: DomainList) -> Arc<Self> {
        let matcher = Matcher::build(&list);
        Arc::new(Self {
            matcher: ArcSwap::from_pointee(matcher),
            list: ArcSwap::from_pointee(list),
            stats: Arc::new(RefreshStats::default()),
        })
    }

    pub fn matcher(&self) -> Arc<Matcher> {
        self.matcher.load_full()
    }

    pub fn list(&self) -> Arc<DomainList> {
        self.list.load_full()
    }

    pub fn stats(&self) -> &Arc<RefreshStats> {
        &self.stats
    }

    /// Replace the live list, if the candidate is usable and actually different.
    ///
    /// Returns whether anything changed.
    pub fn apply(&self, candidate: DomainList) -> bool {
        if candidate.pattern_count() == 0 {
            // Cannot happen through `DomainList::parse`, which rejects an empty list, but this is
            // the last line of defence before a list goes live and the consequence of getting it
            // wrong is the cache silently stopping.
            self.stats.rejected.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        if *self.list.load_full() == candidate {
            self.stats.unchanged.fetch_add(1, Ordering::Relaxed);
            return false;
        }

        // Build the new matcher before publishing anything, so a slow build does not leave
        // readers between two states.
        let matcher = Arc::new(Matcher::build(&candidate));
        self.matcher.store(matcher);
        self.list.store(Arc::new(candidate));
        self.stats.applied.fetch_add(1, Ordering::Relaxed);
        true
    }
}

/// Where a refresh fetches from.
#[derive(Debug, Clone)]
pub struct RefreshSource {
    /// Repository base, e.g. `https://github.com/uklans/cache-domains`.
    pub repo: String,
    pub interval: Duration,
}

impl RefreshSource {
    /// Raw-content base URL for the repository.
    ///
    /// GitHub's web URL is not fetchable as raw files, so it is rewritten. A repository hosted
    /// elsewhere is used as given.
    pub fn raw_base(&self) -> String {
        let repo = self.repo.trim_end_matches('/').trim_end_matches(".git");
        if let Some(path) = repo.strip_prefix("https://github.com/") {
            format!("https://raw.githubusercontent.com/{path}/master")
        } else {
            repo.to_string()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RefreshError {
    #[error("fetching {url}: {source}")]
    Fetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("the fetched list is unusable: {0}")]
    Invalid(#[from] super::domains::DomainsError),
}

/// Fetch a domain list from a repository.
///
/// The whole list is fetched and parsed before anything is applied, so a partial fetch cannot
/// produce a partial list.
pub async fn fetch(
    client: &reqwest::Client,
    source: &RefreshSource,
) -> Result<DomainList, RefreshError> {
    let base = source.raw_base();
    let index_url = format!("{base}/cache_domains.json");
    let index = client
        .get(&index_url)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|source| RefreshError::Fetch {
            url: index_url.clone(),
            source,
        })?
        .text()
        .await
        .map_err(|source| RefreshError::Fetch {
            url: index_url,
            source,
        })?;

    // Parse the index once to learn which files to fetch, then fetch them all before parsing for
    // real.
    let named: serde_json::Value = serde_json::from_str(&index)
        .map_err(|e| RefreshError::Invalid(super::domains::DomainsError::Json(e)))?;
    let mut files = BTreeMap::new();
    if let Some(services) = named["cache_domains"].as_array() {
        for service in services {
            if let Some(names) = service["domain_files"].as_array() {
                for name in names.iter().filter_map(|n| n.as_str()) {
                    if files.contains_key(name) {
                        continue;
                    }
                    let url = format!("{base}/{name}");
                    let body = client
                        .get(&url)
                        .send()
                        .await
                        .and_then(|r| r.error_for_status())
                        .map_err(|source| RefreshError::Fetch {
                            url: url.clone(),
                            source,
                        })?
                        .text()
                        .await
                        .map_err(|source| RefreshError::Fetch { url, source })?;
                    files.insert(name.to_string(), body);
                }
            }
        }
    }

    Ok(DomainList::parse(&index, &files)?)
}

/// Refresh periodically until the process ends.
///
/// A failure is logged and retried at the next interval; it never replaces the live list.
pub fn spawn(
    live: Arc<LiveServices>,
    client: reqwest::Client,
    source: RefreshSource,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if source.interval.is_zero() {
            tracing::info!("domain list refresh disabled");
            return;
        }
        loop {
            tokio::time::sleep(source.interval).await;
            live.stats.attempts.fetch_add(1, Ordering::Relaxed);
            match fetch(&client, &source).await {
                Ok(candidate) => {
                    let services = candidate.services.len();
                    let patterns = candidate.pattern_count();
                    if live.apply(candidate) {
                        tracing::info!(services, patterns, "domain list refreshed");
                    } else {
                        tracing::debug!("domain list unchanged");
                    }
                }
                Err(e) => {
                    live.stats.failed.fetch_add(1, Ordering::Relaxed);
                    // Deliberately not fatal, and deliberately not applied. The previous list
                    // keeps serving.
                    tracing::warn!(error = %e, "domain list refresh failed; keeping the current list");
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(host: &str) -> DomainList {
        let mut files = BTreeMap::new();
        files.insert("x.txt".to_string(), format!("{host}\n"));
        DomainList::parse(
            r#"{"cache_domains":[{"name":"x","domain_files":["x.txt"]}]}"#,
            &files,
        )
        .unwrap()
    }

    #[test]
    fn a_new_list_is_applied_and_visible_immediately() {
        let live = LiveServices::new(list("old.example.com"));
        assert_eq!(live.matcher().service_for("old.example.com"), Some("x"));

        assert!(live.apply(list("new.example.com")));
        assert_eq!(live.matcher().service_for("new.example.com"), Some("x"));
        assert_eq!(live.matcher().service_for("old.example.com"), None);
        assert_eq!(live.stats().applied.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn an_identical_list_is_not_reapplied() {
        // Rebuilding the matcher for an unchanged list is pure waste, and it would make the
        // "applied" metric meaningless as a signal that something actually moved.
        let live = LiveServices::new(list("a.example.com"));
        assert!(!live.apply(list("a.example.com")));
        assert_eq!(live.stats().unchanged.load(Ordering::Relaxed), 1);
        assert_eq!(live.stats().applied.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn an_empty_list_is_rejected_and_the_previous_one_keeps_serving() {
        // The failure this whole design exists to prevent: an automatic update that silently
        // stops the cache caching anything.
        let live = LiveServices::new(list("a.example.com"));
        assert!(!live.apply(DomainList::default()));
        assert_eq!(live.matcher().service_for("a.example.com"), Some("x"));
        assert_eq!(live.stats().rejected.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn readers_never_observe_a_half_applied_list() {
        // The matcher is built before anything is published, so a reader sees the old list or the
        // new one, never a mixture.
        let live = LiveServices::new(list("a.example.com"));
        let reader = live.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..2000 {
                let m = reader.matcher();
                // Exactly one of the two must match, always.
                let a = m.service_for("a.example.com").is_some();
                let b = m.service_for("b.example.com").is_some();
                assert!(a ^ b, "observed a matcher matching both or neither");
            }
        });
        for i in 0..200 {
            live.apply(list(if i % 2 == 0 {
                "b.example.com"
            } else {
                "a.example.com"
            }));
        }
        handle.join().unwrap();
    }

    #[test]
    fn rewrites_a_github_url_to_raw_content() {
        // A GitHub web URL is not fetchable as raw files, and the PRD's default is a web URL.
        let source = RefreshSource {
            repo: "https://github.com/uklans/cache-domains".into(),
            interval: Duration::from_secs(60),
        };
        assert_eq!(
            source.raw_base(),
            "https://raw.githubusercontent.com/uklans/cache-domains/master"
        );
    }

    #[test]
    fn tolerates_a_trailing_slash_or_git_suffix() {
        for repo in [
            "https://github.com/uklans/cache-domains/",
            "https://github.com/uklans/cache-domains.git",
        ] {
            let source = RefreshSource {
                repo: repo.into(),
                interval: Duration::from_secs(60),
            };
            assert_eq!(
                source.raw_base(),
                "https://raw.githubusercontent.com/uklans/cache-domains/master"
            );
        }
    }

    #[test]
    fn a_non_github_repo_is_used_as_given() {
        // An air-gapped site serving the list from its own mirror.
        let source = RefreshSource {
            repo: "https://mirror.internal/cache-domains".into(),
            interval: Duration::from_secs(60),
        };
        assert_eq!(source.raw_base(), "https://mirror.internal/cache-domains");
    }
}
