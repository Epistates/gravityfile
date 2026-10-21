//! Human-readable sizes and ages, and parsers for their command-line forms.

use std::time::Duration;

use serde::{Deserialize, Serialize};

/// How sizes are displayed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Units {
    /// Powers of 1000 (`kB`, `MB`, `GB`), matching Finder and most file managers.
    #[default]
    Si,
    /// Powers of 1024 (`KiB`, `MiB`, `GiB`), matching `du -h`.
    Binary,
}

impl Units {
    fn table(self) -> (f64, [&'static str; 7]) {
        match self {
            Units::Si => (1000.0, ["B", "kB", "MB", "GB", "TB", "PB", "EB"]),
            Units::Binary => (1024.0, ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"]),
        }
    }
}

/// Format a byte count with three significant digits: `999 B`, `1.50 kB`,
/// `52.3 GB`, `140 GB`.
pub fn format_size(bytes: u64, units: Units) -> String {
    let (base, names) = units.table();
    if (bytes as f64) < base {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= base && unit < names.len() - 1 {
        value /= base;
        unit += 1;
    }
    // Rounding can carry into the next unit (999.96 kB -> "1000 kB").
    if value >= 999.5 && unit < names.len() - 1 {
        value /= base;
        unit += 1;
    }
    let name = names[unit];
    if value >= 99.95 {
        format!("{value:.0} {name}")
    } else if value >= 9.995 {
        format!("{value:.1} {name}")
    } else {
        format!("{value:.2} {name}")
    }
}

/// Parse `500`, `100MB`, `1.5 GiB`, `2g`. Decimal units are powers of 1000,
/// `*iB` units powers of 1024. Integer amounts are exact.
pub fn parse_size(input: &str) -> Result<u64, String> {
    let input = input.trim();
    let split = input
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(input.len());
    let (number, unit) = input.split_at(split);
    if number.is_empty() {
        return Err(format!(
            "{input:?} is not a size: expected a non-negative number such as 500MB"
        ));
    }
    let multiplier: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1_000,
        "m" | "mb" => 1_000_000,
        "g" | "gb" => 1_000_000_000,
        "t" | "tb" => 1_000_000_000_000,
        "p" | "pb" => 1_000_000_000_000_000,
        "kib" => 1 << 10,
        "mib" => 1 << 20,
        "gib" => 1 << 30,
        "tib" => 1 << 40,
        "pib" => 1 << 50,
        other => {
            return Err(format!(
                "unknown size unit {other:?}: use B, kB, MB, GB, TB, or KiB, MiB, GiB, TiB"
            ));
        }
    };
    let too_large = || format!("{input:?} is too large");
    if number.bytes().all(|byte| byte.is_ascii_digit()) {
        return number
            .parse::<u64>()
            .ok()
            .and_then(|value| value.checked_mul(multiplier))
            .ok_or_else(too_large);
    }
    let value: f64 = number
        .parse()
        .map_err(|_| format!("{input:?} is not a size: expected a number such as 1.5GB"))?;
    let scaled = value * multiplier as f64;
    if !scaled.is_finite() || scaled >= u64::MAX as f64 {
        return Err(too_large());
    }
    Ok(scaled as u64)
}

/// Format a count with thousands separators: `300,020`.
pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut grouped = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const MONTH: u64 = 30 * DAY;
const YEAR: u64 = 365 * DAY;

/// Format an age in its largest whole unit, `45 minutes`, `3 days`,
/// `11 months`, or in years to one decimal, `1 year`, `2.5 years`. Months
/// and years have the lengths [`parse_age`] gives them, so a parsed `1y`
/// formats as `1 year`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    let whole = |count: u64, unit: &str| {
        let plural = if count == 1 { "" } else { "s" };
        format!("{count} {unit}{plural}")
    };
    match secs {
        0..MINUTE => whole(secs, "second"),
        MINUTE..HOUR => whole(secs / MINUTE, "minute"),
        HOUR..DAY => whole(secs / HOUR, "hour"),
        DAY..MONTH => whole(secs / DAY, "day"),
        MONTH..YEAR => whole(secs / MONTH, "month"),
        _ => match (secs / YEAR, secs % YEAR * 10 / YEAR) {
            (years, 0) => whole(years, "year"),
            (years, tenth) => format!("{years}.{tenth} years"),
        },
    }
}

/// Parse `90min`, `12h`, `30d`, `2w`, `6mo`, `1y`. A bare `m` is rejected as
/// ambiguous between minutes and months.
pub fn parse_age(input: &str) -> Result<Duration, String> {
    let lowered = input.trim().to_ascii_lowercase();
    let split = lowered
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(lowered.len());
    let (number, unit) = lowered.split_at(split);
    let seconds = match unit.trim() {
        "s" | "sec" | "secs" => 1,
        "min" | "mins" => MINUTE,
        "h" | "hr" | "hrs" | "hour" | "hours" => HOUR,
        "d" | "day" | "days" => DAY,
        "w" | "wk" | "week" | "weeks" => 7 * DAY,
        "mo" | "month" | "months" => MONTH,
        "y" | "yr" | "year" | "years" => YEAR,
        "m" => {
            return Err(format!(
                "{input:?} is ambiguous: use \"min\" for minutes or \"mo\" for months"
            ));
        }
        _ => {
            return Err(format!(
                "{input:?} is not an age: expected a number and a unit, such as 30d, 6mo or 1y"
            ));
        }
    };
    let value: f64 = number
        .parse()
        .map_err(|_| format!("{input:?} is not an age: expected a number such as 30d"))?;
    // `try_from_secs_f64` rejects negatives, NaN, infinity and values past
    // `Duration::MAX` exactly; comparing with `Duration::MAX.as_secs_f64()`
    // let 2^64 seconds through, because that bound rounds up to it.
    Duration::try_from_secs_f64(value * seconds as f64)
        .map_err(|_| format!("{input:?} is out of range"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_three_significant_digits() {
        assert_eq!(format_size(0, Units::Si), "0 B");
        assert_eq!(format_size(999, Units::Si), "999 B");
        assert_eq!(format_size(1_500, Units::Si), "1.50 kB");
        assert_eq!(format_size(52_300_000_000, Units::Si), "52.3 GB");
        assert_eq!(format_size(140_000_000_000, Units::Si), "140 GB");
        assert_eq!(format_size(999_960, Units::Si), "1.00 MB");
        assert_eq!(format_size(1023, Units::Binary), "1023 B");
        assert_eq!(format_size(1 << 30, Units::Binary), "1.00 GiB");
        assert_eq!(format_size(u64::MAX, Units::Si), "18.4 EB");
        assert_eq!(format_size(u64::MAX, Units::Binary), "16.0 EiB");
    }

    #[test]
    fn groups_counts_by_thousands() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(300_020), "300,020");
        assert_eq!(format_count(u64::MAX), "18,446,744,073,709,551,615");
    }

    #[test]
    fn parses_decimal_and_binary_sizes() {
        assert_eq!(parse_size("500"), Ok(500));
        assert_eq!(parse_size(" 100MB "), Ok(100_000_000));
        assert_eq!(parse_size("1.5 gb"), Ok(1_500_000_000));
        assert_eq!(parse_size("1GiB"), Ok(1 << 30));
        assert_eq!(parse_size("18446744073709551615"), Ok(u64::MAX));
        for invalid in [
            "",
            "2x",
            "abc",
            "-1",
            "1e400GB",
            "18446744073709551616",
            "20000PB",
        ] {
            assert!(parse_size(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn parses_ages_and_rejects_ambiguous_minutes() {
        assert_eq!(parse_age("30d"), Ok(Duration::from_secs(30 * DAY)));
        assert_eq!(parse_age("6mo"), Ok(Duration::from_secs(180 * DAY)));
        assert_eq!(parse_age("2W"), Ok(Duration::from_secs(14 * DAY)));
        assert_eq!(parse_age("90min"), Ok(Duration::from_secs(90 * MINUTE)));
        assert_eq!(parse_age("1.5h"), Ok(Duration::from_secs(90 * MINUTE)));
        assert!(parse_age("6m").unwrap_err().contains("ambiguous"));
        // 18446744073709551616s is 2^64 seconds, just past `Duration::MAX`;
        // found by fuzzing, it panicked.
        for invalid in ["", "30", "soon", "-1d", "1e400y", "18446744073709551616s"] {
            assert!(parse_age(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn formats_ages_in_the_units_they_parse_in() {
        let secs = Duration::from_secs;
        assert_eq!(format_age(secs(1)), "1 second");
        assert_eq!(format_age(secs(30)), "30 seconds");
        assert_eq!(format_age(secs(2 * MINUTE)), "2 minutes");
        assert_eq!(format_age(secs(DAY)), "1 day");
        assert_eq!(format_age(secs(29 * DAY)), "29 days");
        for (input, expected) in [
            ("1y", "1 year"),
            ("6mo", "6 months"),
            ("1mo", "1 month"),
            ("2.5y", "2.5 years"),
            ("2w", "14 days"),
        ] {
            assert_eq!(format_age(parse_age(input).unwrap()), expected, "{input}");
        }
        assert_eq!(format_age(secs(YEAR - 1)), "12 months");
        assert_eq!(format_age(Duration::MAX), "584942417355 years");
    }
}
