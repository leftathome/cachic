//! Per-service rules loaded from an optional TOML file.
//!
//! Rules are data, not code, so adding a service or correcting its cache key is a configuration
//! change rather than a release (FR-21). The shipped defaults reproduce monolithic's behaviour;
//! this file lets an operator override them.

use std::{collections::BTreeMap, path::Path};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum RulesError {
    #[error("cannot read rules file {path}: {source}")]
    Read {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("rules file {path} is invalid: {source}")]
    Parse {
        path: std::path::PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("service {service:?}: {reason}")]
    Invalid { service: String, reason: String },
}

/// How a service's cache key is derived from a request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ServiceRule {
    /// Keep the query string in the cache key. Off by default: most CDNs put authentication
    /// tokens there, and including them would make every request a miss.
    pub keep_query: bool,

    /// Include the request host in the cache key. Off by default, which is what lets several
    /// CDN hostnames for the same content share one cached copy - the reason lancache exists.
    pub include_host: bool,

    /// Honour upstream `Cache-Control` and `Expires` instead of `CACHE_MAX_AGE` (FR-20).
    pub honour_upstream_cache_control: bool,

    /// Fetch upstream over https rather than matching the client's scheme.
    pub upstream_https: bool,

    /// Slice size override for this service.
    pub slice_size: Option<String>,

    /// Cap on concurrent upstream fetches for this service (FR-09).
    pub max_inflight: Option<usize>,

    /// Only cache paths matching these patterns, if any are given.
    pub include_paths: Vec<String>,

    /// Never cache paths matching these patterns.
    pub exclude_paths: Vec<String>,
}

/// The rules file as a whole.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Rules {
    /// Applied to every service that does not override them.
    pub defaults: ServiceRule,
    /// Keyed by service identifier, as used by `cache-domains`.
    pub services: BTreeMap<String, ServiceRule>,
}

impl Rules {
    pub fn load(path: &Path) -> Result<Self, RulesError> {
        let text = std::fs::read_to_string(path).map_err(|source| RulesError::Read {
            path: path.to_owned(),
            source,
        })?;
        let rules: Rules = toml::from_str(&text).map_err(|source| RulesError::Parse {
            path: path.to_owned(),
            source,
        })?;
        rules.validate()?;
        Ok(rules)
    }

    fn validate(&self) -> Result<(), RulesError> {
        for (name, rule) in &self.services {
            if let Some(slice) = &rule.slice_size {
                let size = super::units::parse_size(slice).map_err(|e| RulesError::Invalid {
                    service: name.clone(),
                    reason: format!("slice_size: {e}"),
                })?;
                if size == 0 || !size.is_power_of_two() {
                    return Err(RulesError::Invalid {
                        service: name.clone(),
                        reason: format!("slice_size {slice:?} must be a power of two"),
                    });
                }
            }
            if rule.max_inflight == Some(0) {
                return Err(RulesError::Invalid {
                    service: name.clone(),
                    reason: "max_inflight must be at least 1".into(),
                });
            }
        }
        Ok(())
    }

    /// The effective rule for a service: its own entry if present, otherwise the defaults.
    pub fn for_service(&self, service: &str) -> &ServiceRule {
        self.services.get(service).unwrap_or(&self.defaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    fn write(dir: &Scratch, body: &str) -> std::path::PathBuf {
        let path = dir.path().join("rules.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn an_absent_file_is_not_the_same_as_an_empty_one() {
        let dir = Scratch::new("rules-missing");
        let err = Rules::load(&dir.path().join("nope.toml")).unwrap_err();
        assert!(matches!(err, RulesError::Read { .. }));
    }

    #[test]
    fn loads_defaults_and_per_service_overrides() {
        let dir = Scratch::new("rules-load");
        let path = write(
            &dir,
            r#"
[defaults]
keep_query = false

[services.steam]
keep_query = true
max_inflight = 32

[services.wsus]
include_host = true
slice_size = "4m"
"#,
        );
        let rules = Rules::load(&path).unwrap();
        assert!(rules.for_service("steam").keep_query);
        assert_eq!(rules.for_service("steam").max_inflight, Some(32));
        assert!(rules.for_service("wsus").include_host);
        // An unknown service falls back to the defaults rather than erroring: cache-domains adds
        // services faster than we can enumerate them.
        assert!(!rules.for_service("something-new").keep_query);
    }

    #[test]
    fn rejects_unknown_keys() {
        // A silently ignored typo in a rules file is a cache that quietly behaves differently
        // from what the operator wrote.
        let dir = Scratch::new("rules-typo");
        let path = write(&dir, "[services.steam]\nkeepquery = true\n");
        let err = Rules::load(&path).unwrap_err();
        assert!(matches!(err, RulesError::Parse { .. }), "{err}");
        assert!(err.to_string().contains("rules.toml"), "{err}");
    }

    #[test]
    fn rejects_an_invalid_slice_size_naming_the_service() {
        let dir = Scratch::new("rules-slice");
        let path = write(&dir, "[services.steam]\nslice_size = \"3m\"\n");
        let err = Rules::load(&path).unwrap_err();
        assert!(err.to_string().contains("steam"), "{err}");
        assert!(err.to_string().contains("power of two"), "{err}");
    }

    #[test]
    fn rejects_zero_max_inflight() {
        let dir = Scratch::new("rules-inflight");
        let path = write(&dir, "[services.steam]\nmax_inflight = 0\n");
        assert!(Rules::load(&path).is_err());
    }

    #[test]
    fn an_empty_file_is_valid_and_means_defaults() {
        let dir = Scratch::new("rules-empty");
        let path = write(&dir, "");
        assert_eq!(Rules::load(&path).unwrap(), Rules::default());
    }
}
