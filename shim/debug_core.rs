// Leveled verbosity, shared (via `include!`/`mod`) between the CLI and
// the dependency-free LD_PRELOAD shim. Pure parsing/ordering, no I/O:
// the shim must never touch stdout/stderr, so sink handling stays
// separate in each binary.

/// Ordered `Error < Warn < Info < Debug < Trace` - a message at `level`
/// is shown whenever `level <= configured_verbosity()`, so raising
/// verbosity shows strictly more than the default `Info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verbosity {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl Verbosity {
    /// The upper-case tag used as a log line's message-type head (see
    /// `format_line`), spelled out explicitly rather than relying on
    /// `{:?}` matching this forever.
    pub fn as_str(self) -> &'static str {
        match self {
            Verbosity::Error => "ERROR",
            Verbosity::Warn => "WARN",
            Verbosity::Info => "INFO",
            Verbosity::Debug => "DEBUG",
            Verbosity::Trace => "TRACE",
        }
    }
}

/// Renders one log line's full head (ISO 8601 UTC timestamp, pid,
/// level) in the shared, greppable shape:
/// `[<iso8601>] [pid <pid>] [<LEVEL>] <message>`. No trailing newline.
pub fn format_line(level: Verbosity, message: &str) -> String {
    format!(
        "[{}] [pid {}] [{}] {message}",
        iso8601_utc_millis(std::time::SystemTime::now()),
        std::process::id(),
        level.as_str()
    )
}

/// Renders `time` as `YYYY-MM-DDTHH:MM:SS.mmmZ` (ISO 8601, UTC,
/// millisecond precision), hand-rolled since the shim can't link
/// `chrono`/`time`. Takes `time` explicitly to stay pure/testable.
fn iso8601_utc_millis(time: std::time::SystemTime) -> String {
    let since_epoch = time
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let millis = since_epoch.as_millis();
    let secs = (millis / 1000) as i64;
    let ms = (millis % 1000) as u32;
    let days = secs.div_euclid(86400);
    let secs_of_day = secs.rem_euclid(86400);
    let (year, month, day) = civil_from_days(days);
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

/// Howard Hinnant's `civil_from_days` algorithm
/// (<https://howardhinnant.github.io/date_algorithms.html>, public
/// domain): days-since-1970-01-01 -> `(year, month, day)`, correct for
/// any `z` including negative (pre-1970) values.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32; // [1, 12]
    (y + i64::from(m <= 2), m, d)
}

/// Parses one of the five lowercase level names (case-insensitive).
/// `None` for anything else, including empty; `configured_verbosity`
/// is the one place that degrades an unparseable value to a default.
pub fn parse_verbosity(value: &str) -> Option<Verbosity> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Some(Verbosity::Error),
        "warn" => Some(Verbosity::Warn),
        "info" => Some(Verbosity::Info),
        "debug" => Some(Verbosity::Debug),
        "trace" => Some(Verbosity::Trace),
        _ => None,
    }
}

/// `GHOSTVOLUMES_DEBUG`'s value, parsed - unset, empty, or unrecognized
/// (including the old `"1"`/`"0"` convention) all degrade to `Info`,
/// the same never-panic, sane-default posture used elsewhere.
pub fn configured_verbosity() -> Verbosity {
    std::env::var("GHOSTVOLUMES_DEBUG")
        .ok()
        .as_deref()
        .and_then(parse_verbosity)
        .unwrap_or(Verbosity::Info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_verbosity_recognizes_every_level_case_insensitively() {
        assert_eq!(parse_verbosity("error"), Some(Verbosity::Error));
        assert_eq!(parse_verbosity("WARN"), Some(Verbosity::Warn));
        assert_eq!(parse_verbosity("Info"), Some(Verbosity::Info));
        assert_eq!(parse_verbosity("debug"), Some(Verbosity::Debug));
        assert_eq!(parse_verbosity("TRACE"), Some(Verbosity::Trace));
    }

    #[test]
    fn parse_verbosity_none_for_the_old_boolean_convention_and_garbage() {
        assert_eq!(parse_verbosity("1"), None);
        assert_eq!(parse_verbosity("0"), None);
        assert_eq!(parse_verbosity(""), None);
        assert_eq!(parse_verbosity("garbage"), None);
    }

    #[test]
    fn ordering_places_error_and_warn_below_the_default_info() {
        assert!(Verbosity::Error < Verbosity::Info);
        assert!(Verbosity::Warn < Verbosity::Info);
        assert!(Verbosity::Debug > Verbosity::Info);
        assert!(Verbosity::Trace > Verbosity::Debug);
    }

    #[test]
    fn as_str_spells_out_the_upper_case_level_name() {
        assert_eq!(Verbosity::Error.as_str(), "ERROR");
        assert_eq!(Verbosity::Warn.as_str(), "WARN");
        assert_eq!(Verbosity::Info.as_str(), "INFO");
        assert_eq!(Verbosity::Debug.as_str(), "DEBUG");
        assert_eq!(Verbosity::Trace.as_str(), "TRACE");
    }

    #[test]
    fn format_line_carries_a_timestamp_pid_level_and_the_message_verbatim() {
        let line = format_line(Verbosity::Debug, "hello world");
        assert!(line.contains("[DEBUG]"));
        assert!(line.contains(&format!("[pid {}]", std::process::id())));
        assert!(line.ends_with("hello world"));
        // An ISO 8601 UTC timestamp: the first bracketed token.
        let first = line.split(']').next().unwrap().trim_start_matches('[');
        assert_eq!(first.len(), "2024-01-01T00:00:00.000Z".len());
        assert!(first.ends_with('Z'));
    }

    #[test]
    fn format_line_has_no_trailing_newline() {
        assert!(!format_line(Verbosity::Info, "x").ends_with('\n'));
    }

    #[test]
    fn iso8601_utc_millis_epoch_zero() {
        assert_eq!(
            iso8601_utc_millis(std::time::UNIX_EPOCH),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn iso8601_utc_millis_a_known_recent_date_with_milliseconds() {
        // 2024-01-01T00:00:00Z is the well-known epoch second
        // 1704067200; +500ms exercises the millisecond component too.
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1704067200500);
        assert_eq!(iso8601_utc_millis(time), "2024-01-01T00:00:00.500Z");
    }

    #[test]
    fn iso8601_utc_millis_end_of_a_leap_year_day() {
        // 2024-02-29T23:59:59Z - epoch second 1709251199 - exercises
        // both the leap-day and end-of-day boundary.
        let time = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_709_251_199);
        assert_eq!(iso8601_utc_millis(time), "2024-02-29T23:59:59.000Z");
    }

    #[test]
    fn a_message_at_a_level_is_shown_exactly_when_it_is_at_or_below_configured() {
        let configured = Verbosity::Info;
        assert!(Verbosity::Error <= configured);
        assert!(Verbosity::Warn <= configured);
        assert!(Verbosity::Info <= configured);
        assert!(!(Verbosity::Debug <= configured));
        assert!(!(Verbosity::Trace <= configured));
    }
}
