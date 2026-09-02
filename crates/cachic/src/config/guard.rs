//! The on-disk configuration guard.
//!
//! `CACHE_DATA_DIR/CONFIG` records the settings that the stored data depends on. If they change,
//! the slices already on disk no longer mean what the running configuration says they mean, so
//! startup aborts rather than serving a mixture.
//!
//! This mirrors monolithic's `CONFIGHASH` behaviour (FR-10). There is deliberately no "warn and
//! continue" mode: a cache that silently reinterprets its own contents is worse than one that
//! refuses to start.

use std::path::{Path, PathBuf};

/// Bumped whenever the on-disk slice format changes incompatibly.
pub const STORE_FORMAT_VERSION: u32 = 1;

const FILE_NAME: &str = "CONFIG";

/// The settings the stored data depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredConfig {
    pub slice_size: u64,
    pub store_format_version: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum GuardError {
    #[error("cannot read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is malformed at line {line}: {reason}")]
    Malformed {
        path: PathBuf,
        line: usize,
        reason: String,
    },
    #[error(
        "cache directory was created with {found}, but this process is configured with \
         {configured}.\n\
         The slices already stored there were written under the old setting and cannot be \
         reinterpreted.\n\
         Either restore the previous setting, point CACHE_DATA_DIR at an empty directory, or set \
         FORCE_CONFIG=true to discard the existing cache."
    )]
    Mismatch { found: String, configured: String },
}

impl StoredConfig {
    fn render(&self) -> String {
        format!(
            "# cachic cache directory configuration.\n\
             # Written automatically. Changing these by hand will not migrate the stored data.\n\
             store_format_version={}\n\
             slice_size={}\n",
            self.store_format_version, self.slice_size
        )
    }

    fn parse(text: &str, path: &Path) -> Result<Self, GuardError> {
        let mut slice_size = None;
        let mut store_format_version = None;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| GuardError::Malformed {
                path: path.to_owned(),
                line: index + 1,
                reason: format!("expected key=value, found {line:?}"),
            })?;
            let parsed = |v: &str| -> Result<u64, GuardError> {
                v.trim().parse().map_err(|_| GuardError::Malformed {
                    path: path.to_owned(),
                    line: index + 1,
                    reason: format!("{:?} is not a number", v.trim()),
                })
            };
            match key.trim() {
                "slice_size" => slice_size = Some(parsed(value)?),
                "store_format_version" => store_format_version = Some(parsed(value)? as u32),
                other => {
                    return Err(GuardError::Malformed {
                        path: path.to_owned(),
                        line: index + 1,
                        reason: format!("unknown key {other:?}"),
                    })
                }
            }
        }
        match (slice_size, store_format_version) {
            (Some(slice_size), Some(store_format_version)) => Ok(Self {
                slice_size,
                store_format_version,
            }),
            _ => Err(GuardError::Malformed {
                path: path.to_owned(),
                line: 0,
                reason: "missing slice_size or store_format_version".into(),
            }),
        }
    }

    fn describe(&self) -> String {
        format!(
            "slice size {} (format version {})",
            super::units::format_size(self.slice_size),
            self.store_format_version
        )
    }
}

/// Check the guard, writing it if the directory is new.
///
/// `force` corresponds to `FORCE_CONFIG=true`: it discards the recorded settings and adopts the
/// current ones. It does not delete the stored slices - the store's own format check does that -
/// but it does mean they will not be found under the new slice size.
pub fn check(dir: &Path, configured: &StoredConfig, force: bool) -> Result<(), GuardError> {
    let path = dir.join(FILE_NAME);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(StoredConfig::parse(&text, &path)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(GuardError::Read {
                path: path.clone(),
                source,
            })
        }
    };

    match existing {
        Some(found) if &found == configured => Ok(()),
        Some(found) if !force => Err(GuardError::Mismatch {
            found: found.describe(),
            configured: configured.describe(),
        }),
        // Either the directory is new, or FORCE_CONFIG overrode a mismatch. Record the current
        // settings so the next start compares against them.
        _ => {
            std::fs::create_dir_all(dir).map_err(|source| GuardError::Write {
                path: path.clone(),
                source,
            })?;
            std::fs::write(&path, configured.render()).map_err(|source| GuardError::Write {
                path: path.clone(),
                source,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Scratch;

    fn config(slice_size: u64) -> StoredConfig {
        StoredConfig {
            slice_size,
            store_format_version: STORE_FORMAT_VERSION,
        }
    }

    #[test]
    fn writes_the_guard_for_a_new_directory() {
        let dir = Scratch::new("guard-new");
        check(dir.path(), &config(1 << 20), false).unwrap();
        let text = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(text.contains("slice_size=1048576"));
        assert!(text.contains(&format!("store_format_version={STORE_FORMAT_VERSION}")));
    }

    #[test]
    fn accepts_an_unchanged_configuration() {
        let dir = Scratch::new("guard-same");
        check(dir.path(), &config(1 << 20), false).unwrap();
        check(dir.path(), &config(1 << 20), false).unwrap();
    }

    #[test]
    fn refuses_a_changed_slice_size() {
        let dir = Scratch::new("guard-changed");
        check(dir.path(), &config(1 << 20), false).unwrap();
        let err = check(dir.path(), &config(4 << 20), false).unwrap_err();
        assert!(matches!(err, GuardError::Mismatch { .. }));
        // The message has to tell an operator what to actually do.
        let text = err.to_string();
        assert!(text.contains("1 MiB"), "{text}");
        assert!(text.contains("4 MiB"), "{text}");
        assert!(text.contains("FORCE_CONFIG"), "{text}");
    }

    #[test]
    fn force_adopts_the_new_configuration() {
        let dir = Scratch::new("guard-force");
        check(dir.path(), &config(1 << 20), false).unwrap();
        check(dir.path(), &config(4 << 20), true).unwrap();
        // And the new value sticks, so the next start without force is happy.
        check(dir.path(), &config(4 << 20), false).unwrap();
        // ... while the old one is now the mismatch.
        assert!(check(dir.path(), &config(1 << 20), false).is_err());
    }

    #[test]
    fn refuses_a_changed_store_format() {
        let dir = Scratch::new("guard-format");
        check(dir.path(), &config(1 << 20), false).unwrap();
        let newer = StoredConfig {
            slice_size: 1 << 20,
            store_format_version: STORE_FORMAT_VERSION + 1,
        };
        assert!(matches!(
            check(dir.path(), &newer, false).unwrap_err(),
            GuardError::Mismatch { .. }
        ));
    }

    #[test]
    fn reports_malformed_guard_files_with_a_line_number() {
        let dir = Scratch::new("guard-malformed");
        std::fs::write(
            dir.path().join(FILE_NAME),
            "store_format_version=1\nslice_size=not-a-number\n",
        )
        .unwrap();
        let err = check(dir.path(), &config(1 << 20), false).unwrap_err();
        match err {
            GuardError::Malformed { line, .. } => assert_eq!(line, 2),
            other => panic!("expected Malformed, got {other:?}"),
        }
    }

    #[test]
    fn ignores_comments_and_blank_lines() {
        let dir = Scratch::new("guard-comments");
        std::fs::write(
            dir.path().join(FILE_NAME),
            "# a comment\n\n  store_format_version = 1 \nslice_size = 1048576\n\n",
        )
        .unwrap();
        check(dir.path(), &config(1 << 20), false).unwrap();
    }
}
