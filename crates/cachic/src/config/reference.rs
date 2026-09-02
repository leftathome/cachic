//! Generation of the configuration reference.
//!
//! The reference is derived from the same `clap` definitions the binary parses, so it cannot
//! describe a flag that does not exist or miss one that does. A hand-maintained config document
//! drifts, and a config document that drifts is worse than none: it tells operators confident
//! things that are false.
//!
//! `docs/configuration.md` is the committed output. A test regenerates it and fails if it differs,
//! so the reference cannot silently fall behind the code.

use clap::CommandFactory;

use super::Config;

/// Render the configuration reference as Markdown.
pub fn render() -> String {
    let command = Config::command();
    let mut out = String::new();

    out.push_str("# Configuration reference\n\n");
    out.push_str(
        "Generated from the command-line definitions; do not edit by hand. Regenerate with:\n\n\
         ```sh\n\
         cargo run --example config-reference > docs/configuration.md\n\
         ```\n\n\
         Every setting can be given as an environment variable or as a command-line flag. \
         Precedence is environment > file > defaults.\n\n\
         Sizes accept nginx spellings (`1000g`, `2g`, `1m`) which are binary multiples, matching \
         `lancachenet/monolithic`, as well as IEC (`GiB`) and SI (`GB`, decimal). Durations accept \
         `s`, `m`, `h`, `d`, `w`.\n\n",
    );
    out.push_str("| Environment variable | Flag | Default | Description |\n");
    out.push_str("|---|---|---|---|\n");

    for arg in command.get_arguments() {
        if arg.is_hide_set() || arg.get_id() == "help" || arg.get_id() == "version" {
            continue;
        }
        let env = arg
            .get_env()
            .map(|e| format!("`{}`", e.to_string_lossy()))
            .unwrap_or_else(|| "-".into());
        let flag = arg
            .get_long()
            .map(|l| format!("`--{l}`"))
            .unwrap_or_else(|| "-".into());
        let default = {
            let values = arg.get_default_values();
            if values.is_empty() {
                "-".to_string()
            } else {
                let joined = values
                    .iter()
                    .map(|v| v.to_string_lossy().to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                if joined.is_empty() {
                    "-".to_string()
                } else {
                    format!("`{joined}`")
                }
            }
        };
        // Take the long help when there is one: the short help is a summary, and the reference is
        // where the reasoning belongs.
        let help = arg
            .get_long_help()
            .or_else(|| arg.get_help())
            .map(|h| h.to_string())
            .unwrap_or_default()
            .replace('\n', " ")
            .replace('|', "\\|");
        out.push_str(&format!("| {env} | {flag} | {default} | {help} |\n"));
    }

    out.push_str(
        "\n## Sizing notes\n\n\
         - **Per-connection memory** is `READAHEAD_SLICES * CACHE_SLICE_SIZE`. At the defaults that \
         is 4 MiB per in-flight client request, and it is a hard bound rather than a target.\n\
         - **Index memory** is roughly 400 bytes per stored slice, and is *not* part of \
         `CACHE_MEM_SIZE`. A 2 TB cache at 1 MiB slices needs about 760 MB for the index; at 4 MiB \
         slices, about 190 MB. `lancachenet/monolithic`'s equivalent figure is around 128 bytes per \
         slice, so budget roughly three times what you would have under nginx.\n\
         - **Changing `CACHE_SLICE_SIZE`** against an existing cache directory aborts startup. The \
         stored slices were written under the old size and cannot be reinterpreted. Set \
         `FORCE_CONFIG=true` to adopt the new size and abandon the existing cache.\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The committed reference must match what the code would generate.
    #[test]
    fn committed_reference_is_current() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/configuration.md");
        let committed = std::fs::read_to_string(path).unwrap_or_default();
        let generated = render();
        assert_eq!(
            committed.trim(),
            generated.trim(),
            "docs/configuration.md is out of date with the config definitions.\n\
             Regenerate it:\n\
             \n    cargo run --example config-reference > docs/configuration.md\n"
        );
    }

    #[test]
    fn every_setting_appears_with_its_environment_variable() {
        let rendered = render();
        for env in [
            "CACHE_DISK_SIZE",
            "CACHE_MEM_SIZE",
            "CACHE_MAX_AGE",
            "CACHE_SLICE_SIZE",
            "CACHE_DATA_DIR",
            "MIN_FREE_DISK",
            "FORCE_CONFIG",
            "HTTP_PORT",
            "HTTPS_PORT",
            "ADMIN_PORT",
            "UPSTREAM_DNS",
            "UPSTREAM_MAX_INFLIGHT",
            "READAHEAD_SLICES",
            "PASSTHROUGH_UNKNOWN_HOSTS",
            "CACHE_DOMAINS_REPO",
            "CACHE_DOMAINS_REFRESH",
            "LOG_FORMAT",
            "LOG_LEVEL",
        ] {
            assert!(rendered.contains(env), "reference omits {env}");
        }
    }
}
