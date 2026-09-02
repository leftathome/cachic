//! Default per-service rules, reproducing `lancachenet/monolithic` (G1, FR-21).
//!
//! These are not invented. Each is transcribed from monolithic's nginx configuration under
//! `overlay/etc/nginx/sites-available/cache.conf.d/`, and the source file is named against each
//! one so a future reviewer can check the transcription rather than trust it.
//!
//! Everything here is an *exclusion*: content monolithic deliberately does not cache, generally
//! because it is a small file that changes often and whose staleness breaks a client. Caching a
//! certificate revocation list or a version manifest is worse than not caching at all.

use std::collections::BTreeMap;

use crate::config::rules::{Rules, ServiceRule};

/// Rules shipped by default, before any operator file is applied.
pub fn shipped() -> Rules {
    let mut services = BTreeMap::new();

    // 20_lol.conf:
    //   location ~ ^.+(releaselisting_.*|.version$) { proxy_pass http://$host; }
    // Riot's release listings and version files: small, frequently changed, and a stale one sends
    // a client after content that no longer exists.
    services.insert(
        "riot".to_string(),
        ServiceRule {
            exclude_paths: vec![r"releaselisting_".into(), r"\.version$".into()],
            ..Default::default()
        },
    );

    // 21_arenanet_manifest.conf:
    //   location ^~ /latest64 { proxy_cache_bypass 1; proxy_no_cache 1; }
    services.insert(
        "arenanet".to_string(),
        ServiceRule {
            exclude_paths: vec![r"^/latest64".into()],
            ..Default::default()
        },
    );

    // 22_wsus_cabs.conf:
    //   location ~* (authrootstl.cab|pinrulesstl.cab|disallowedcertstl.cab)$ { no cache }
    // Certificate trust lists. A stale revocation list is a security problem, not a cache miss.
    services.insert(
        "wsus".to_string(),
        ServiceRule {
            exclude_paths: vec![
                r"(?i)authrootstl\.cab$".into(),
                r"(?i)pinrulesstl\.cab$".into(),
                r"(?i)disallowedcertstl\.cab$".into(),
            ],
            ..Default::default()
        },
    );

    // 23_steam_server_status.conf:
    //   location = /server-status { proxy_no_cache 1; proxy_cache_bypass 1; }
    services.insert(
        "steam".to_string(),
        ServiceRule {
            exclude_paths: vec![r"^/server-status$".into()],
            ..Default::default()
        },
    );

    Rules {
        defaults: ServiceRule::default(),
        services,
    }
}

/// Merge an operator's rules over the shipped defaults.
///
/// An operator entry replaces the shipped one for that service outright rather than merging field
/// by field. Half-overriding a rule set produces a configuration nobody wrote and nobody can
/// predict; replacing it is at least legible.
pub fn merge(operator: Rules) -> Rules {
    let mut merged = shipped();
    merged.defaults = operator.defaults;
    for (service, rule) in operator.services {
        merged.services.insert(service, rule);
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::key::CompiledRule;

    fn compiled(service: &str) -> CompiledRule {
        CompiledRule::compile(service, shipped().for_service(service)).unwrap()
    }

    #[test]
    fn every_shipped_rule_compiles() {
        // A rule with a bad regex would fail at startup, which is a poor time to find out.
        for service in shipped().services.keys() {
            CompiledRule::compile(service, shipped().for_service(service))
                .unwrap_or_else(|e| panic!("service {service}: {e}"));
        }
    }

    #[test]
    fn riot_release_listings_are_not_cached() {
        // 20_lol.conf
        let rule = compiled("riot");
        assert!(!rule.is_cacheable("/releases/releaselisting_EUW"));
        assert!(!rule.is_cacheable("/projects/lol/releases/0.0.1.version"));
        // Ordinary content still caches.
        assert!(rule.is_cacheable("/releases/patcher/content.bin"));
    }

    #[test]
    fn arenanet_latest64_is_not_cached() {
        // 21_arenanet_manifest.conf. Prefix-anchored, as `location ^~` is.
        let rule = compiled("arenanet");
        assert!(!rule.is_cacheable("/latest64"));
        assert!(!rule.is_cacheable("/latest64/manifest"));
        assert!(rule.is_cacheable("/program/data.dat"));
        // Not anchored mid-path: only a prefix match counts.
        assert!(rule.is_cacheable("/other/latest64"));
    }

    #[test]
    fn wsus_certificate_trust_lists_are_not_cached() {
        // 22_wsus_cabs.conf. A stale revocation list is a security problem, not a cache miss.
        let rule = compiled("wsus");
        for name in [
            "authrootstl.cab",
            "pinrulesstl.cab",
            "disallowedcertstl.cab",
        ] {
            assert!(!rule.is_cacheable(&format!("/msdownload/update/v3/static/trustedr/en/{name}")));
            // monolithic matches case-insensitively (`location ~*`).
            assert!(!rule.is_cacheable(&format!("/x/{}", name.to_uppercase())));
        }
        assert!(rule.is_cacheable("/msdownload/update/software/windows10.0-kb.msu"));
    }

    #[test]
    fn steam_server_status_is_not_cached() {
        // 23_steam_server_status.conf. An exact match, as `location =` is.
        let rule = compiled("steam");
        assert!(!rule.is_cacheable("/server-status"));
        assert!(rule.is_cacheable("/server-status/detail"));
        assert!(rule.is_cacheable("/depot/440/chunk"));
    }

    #[test]
    fn services_with_no_special_rules_cache_everything() {
        // The majority. Most of cache-domains needs nothing beyond the defaults.
        let rules = shipped();
        for service in ["blizzard", "epicgames", "sony", "nintendo", "xboxlive"] {
            let rule = CompiledRule::compile(service, rules.for_service(service)).unwrap();
            assert!(rule.is_cacheable("/anything/at/all"));
            assert!(!rule.keep_query, "{service} should drop the query string");
            assert!(!rule.include_host, "{service} should exclude the host");
        }
    }

    #[test]
    fn an_operator_rule_replaces_the_shipped_one_outright() {
        // Field-by-field merging would produce a configuration nobody wrote.
        let mut operator = Rules::default();
        operator.services.insert(
            "steam".into(),
            ServiceRule {
                keep_query: true,
                ..Default::default()
            },
        );
        let merged = merge(operator);
        let steam = merged.for_service("steam");
        assert!(steam.keep_query);
        assert!(
            steam.exclude_paths.is_empty(),
            "the shipped exclusion leaked into an overridden rule"
        );
        // Other services keep their shipped rules.
        assert!(!merged.for_service("wsus").exclude_paths.is_empty());
    }

    #[test]
    fn the_shipped_set_covers_exactly_the_services_monolithic_special_cases() {
        // If monolithic gains or loses a special case, this should be reviewed rather than
        // silently drifting.
        let names: Vec<_> = shipped().services.keys().cloned().collect();
        assert_eq!(names, vec!["arenanet", "riot", "steam", "wsus"]);
    }
}
