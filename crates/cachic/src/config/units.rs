//! Size and duration parsing for the configuration surface.
//!
//! Operators arriving from `lancachenet/monolithic` write `1000g` and `3560d`, so those spellings
//! must work exactly as nginx reads them (binary multiples, not decimal). Unambiguous IEC and SI
//! spellings are accepted too, because `1000g` meaning gibibytes surprises people who did not come
//! from nginx.
//!
//! Everything here is pure and total: it either returns a value or an error that names the input.
//! This is one of the fuzz targets in TASK-21, so it is written to be fuzzed - no panics, no
//! unwraps, no arithmetic that can overflow silently.

use std::time::Duration;

/// Why a size or duration string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("empty value")]
    Empty,
    #[error("{0:?} is not a number followed by an optional unit")]
    Malformed(String),
    #[error("unknown unit {unit:?} in {input:?}")]
    UnknownUnit { input: String, unit: String },
    #[error("{0:?} overflows")]
    Overflow(String),
}

/// Split a value into its numeric prefix and unit suffix.
fn split(input: &str) -> Result<(u64, &str), ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::Empty);
    }
    let digits = trimmed
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(trimmed.len());
    if digits == 0 {
        return Err(ParseError::Malformed(trimmed.to_owned()));
    }
    let value: u64 = trimmed[..digits]
        .parse()
        .map_err(|_| ParseError::Overflow(trimmed.to_owned()))?;
    Ok((value, trimmed[digits..].trim()))
}

/// Parse a byte size.
///
/// nginx spellings (`k`, `m`, `g`, `t`) are binary multiples, matching monolithic. IEC (`KiB`,
/// `MiB`, ...) and SI (`KB`, `MB`, ...) are also accepted, and SI means powers of ten.
pub fn parse_size(input: &str) -> Result<u64, ParseError> {
    let (value, unit) = split(input)?;
    let multiplier: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        // nginx-style and IEC: binary.
        "k" | "kib" => 1 << 10,
        "m" | "mib" => 1 << 20,
        "g" | "gib" => 1 << 30,
        "t" | "tib" => 1 << 40,
        // SI: decimal, because someone writing GB usually means GB.
        "kb" => 1_000,
        "mb" => 1_000_000,
        "gb" => 1_000_000_000,
        "tb" => 1_000_000_000_000,
        other => {
            return Err(ParseError::UnknownUnit {
                input: input.trim().to_owned(),
                unit: other.to_owned(),
            })
        }
    };
    value
        .checked_mul(multiplier)
        .ok_or_else(|| ParseError::Overflow(input.trim().to_owned()))
}

/// Parse a duration in nginx spelling: `s`, `m` (minutes), `h`, `d`, `w`. Bare numbers are seconds.
pub fn parse_duration(input: &str) -> Result<Duration, ParseError> {
    let (value, unit) = split(input)?;
    let seconds: u64 = match unit.to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" => 1,
        "m" | "min" | "mins" => 60,
        "h" | "hr" | "hrs" => 3_600,
        "d" | "day" | "days" => 86_400,
        "w" | "week" | "weeks" => 604_800,
        other => {
            return Err(ParseError::UnknownUnit {
                input: input.trim().to_owned(),
                unit: other.to_owned(),
            })
        }
    };
    value
        .checked_mul(seconds)
        .map(Duration::from_secs)
        .ok_or_else(|| ParseError::Overflow(input.trim().to_owned()))
}

/// Render a byte count the way the config reference and logs should show it.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("TiB", 1 << 40),
        ("GiB", 1 << 30),
        ("MiB", 1 << 20),
        ("KiB", 1 << 10),
    ];
    for (name, mult) in UNITS {
        if bytes >= mult && bytes.is_multiple_of(mult) {
            return format!("{} {name}", bytes / mult);
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_prd_defaults() {
        // Every default in PRD section 8 must parse.
        assert_eq!(parse_size("1000g").unwrap(), 1000 * (1 << 30));
        assert_eq!(parse_size("2g").unwrap(), 2 * (1 << 30));
        assert_eq!(parse_size("1m").unwrap(), 1 << 20);
        assert_eq!(parse_size("10g").unwrap(), 10 * (1 << 30));
        assert_eq!(parse_duration("3560d").unwrap().as_secs(), 3560 * 86_400);
        assert_eq!(parse_duration("24h").unwrap().as_secs(), 86_400);
    }

    #[test]
    fn nginx_units_are_binary() {
        // This is the compatibility point: an operator's existing 1000g must mean what it meant
        // under monolithic, not 7% less.
        assert_eq!(parse_size("1g").unwrap(), 1_073_741_824);
        assert_eq!(parse_size("1k").unwrap(), 1024);
    }

    #[test]
    fn si_units_are_decimal() {
        assert_eq!(parse_size("1GB").unwrap(), 1_000_000_000);
        assert_eq!(parse_size("1gb").unwrap(), 1_000_000_000);
        assert_ne!(parse_size("1GB").unwrap(), parse_size("1g").unwrap());
    }

    #[test]
    fn iec_units_match_nginx_units() {
        assert_eq!(parse_size("1GiB").unwrap(), parse_size("1g").unwrap());
        assert_eq!(parse_size("512MiB").unwrap(), parse_size("512m").unwrap());
    }

    #[test]
    fn bare_numbers_are_bytes_and_seconds() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_duration("90").unwrap().as_secs(), 90);
    }

    #[test]
    fn whitespace_is_tolerated() {
        assert_eq!(parse_size("  16 MiB ").unwrap(), 16 << 20);
        assert_eq!(parse_duration(" 7 d ").unwrap().as_secs(), 7 * 86_400);
    }

    #[test]
    fn rejects_malformed_input_without_panicking() {
        for bad in ["", "  ", "g", "abc", "-1", "1.5g", "1x", "m1", "1 2 g"] {
            assert!(parse_size(bad).is_err(), "size {bad:?} should not parse");
        }
        for bad in ["", "d", "abc", "-1", "1.5h", "1y"] {
            assert!(
                parse_duration(bad).is_err(),
                "duration {bad:?} should not parse"
            );
        }
    }

    #[test]
    fn reports_overflow_rather_than_wrapping() {
        // A silently wrapped cache size would configure a tiny cache from a huge number.
        let err = parse_size("99999999999999999999g").unwrap_err();
        assert!(matches!(err, ParseError::Overflow(_)), "got {err:?}");
        let err = parse_size("18446744073709551615t").unwrap_err();
        assert!(matches!(err, ParseError::Overflow(_)), "got {err:?}");
    }

    #[test]
    fn errors_name_the_offending_input() {
        let err = parse_size("5quatloos").unwrap_err();
        let text = err.to_string();
        assert!(text.contains("quatloos"), "unhelpful error: {text}");
        assert!(text.contains("5quatloos"), "error omits the input: {text}");
    }

    #[test]
    fn formats_sizes_readably() {
        assert_eq!(format_size(1 << 30), "1 GiB");
        assert_eq!(format_size(1536 << 20), "1536 MiB");
        assert_eq!(format_size(1000), "1000 B");
    }

    #[test]
    fn never_panics_on_arbitrary_input() {
        // Cheap stand-in for the fuzz target in TASK-21: the parsers are total.
        let mut seed = 0x1234_5678_9abc_def0u64;
        for _ in 0..20_000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed % 12) as usize;
            let s: String = (0..len)
                .map(|i| {
                    let b = ((seed >> (i % 8 * 8)) & 0xff) as u8;
                    // Bias towards characters that appear in real values.
                    match b % 5 {
                        0 => (b'0' + b % 10) as char,
                        1 => (b'a' + b % 26) as char,
                        2 => ' ',
                        3 => (b'A' + b % 26) as char,
                        _ => b as char,
                    }
                })
                .collect();
            let _ = parse_size(&s);
            let _ = parse_duration(&s);
        }
    }
}
