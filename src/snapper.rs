//! The few Snapper facts `prune --since` needs, from Snapper's own
//! machine-readable CLI (never its XML): `snapper --jsonout --utc -c <c>
//! list` (`--jsonout`/`--columns` since 0.8.6, the `read-only` column
//! since 0.10.5; we require 0.12+) and `get-config` (`SUBVOLUME`).

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

/// One listed snapshot, as far as baseline selection cares.
#[derive(Debug, Clone, PartialEq)]
pub struct Snap {
    pub number: u64,
    pub date: SystemTime,
    pub description: String,
    /// `userdata.verify`; `None` for untagged (Snapper prints `null`
    /// userdata when there's none).
    pub verify: Option<String>,
    pub read_only: bool,
}

/// `verify=` results whose snapshot was pruned under its own decisions
/// and may serve as the "before" side (never `pending`/`failed`).
const BASELINE_TAGS: &[&str] = &[
    "ok",
    "refused",
    "rules-changed",
    "recovered",
    "health-failed",
    "health-timeout",
    "manifest-changed",
];

/// Snapper config names we pass on: no option look-alikes, no paths.
pub fn valid_config(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
}

/// `YYYY-MM-DD`, `YYYY-MM-DD HH:MM` or `YYYY-MM-DD HH:MM:SS`, as UTC —
/// Snapper's machine-readable `"%F %T"` with `--utc`, and the exact form
/// of `--since`. Anything else is `None`.
pub fn parse_utc(text: &str) -> Option<SystemTime> {
    let shape_ok = |s: &str, pattern: &str| {
        s.len() == pattern.len()
            && s.bytes().zip(pattern.bytes()).all(|(c, p)| match p {
                b'9' => c.is_ascii_digit(),
                _ => c == p,
            })
    };
    let full = if shape_ok(text, "9999-99-99") {
        format!("{text}T00:00:00Z")
    } else if shape_ok(text, "9999-99-99 99:99") {
        format!("{}:00Z", text.replace(' ', "T"))
    } else if shape_ok(text, "9999-99-99 99:99:99") {
        format!("{}Z", text.replace(' ', "T"))
    } else {
        return None;
    };
    humantime::parse_rfc3339(&full).ok()
}

/// A `--since` expression.
#[derive(Debug, PartialEq)]
pub enum Since {
    /// Before the pruned snapshot's own date (`1d`, `6h`, …).
    Delta(Duration),
    /// An exact UTC time.
    At(SystemTime),
}

pub fn parse_since(expr: &str) -> anyhow::Result<Since> {
    if let Some(at) = parse_utc(expr) {
        return Ok(Since::At(at));
    }
    humantime::parse_duration(expr)
        .map(Since::Delta)
        .map_err(|_| {
            anyhow::anyhow!(
                "--since {expr:?}: expected a duration (1d, 6h) or a UTC time \
                 (YYYY-MM-DD[ HH:MM[:SS]])"
            )
        })
}

/// `snapper --jsonout list` output → the snapshots of `config` (only that
/// top-level key). Snapshot 0 ("current", empty date) is skipped.
pub fn parse_list(json: &[u8], config: &str) -> anyhow::Result<Vec<Snap>> {
    let value: serde_json::Value = serde_json::from_slice(json)?;
    let rows = value
        .get(config)
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("no snapshot list for config {config:?}"))?;
    let mut out = Vec::new();
    for row in rows {
        let number = row
            .get("number")
            .and_then(|v| v.as_u64())
            .ok_or_else(|| anyhow::anyhow!("a snapshot without a number"))?;
        if number == 0 {
            continue;
        }
        let date = row
            .get("date")
            .and_then(|v| v.as_str())
            .and_then(parse_utc)
            .ok_or_else(|| anyhow::anyhow!("snapshot {number}: no usable date"))?;
        let verify = match row.get("userdata") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Object(map)) => map
                .get("verify")
                .and_then(|v| v.as_str())
                .map(str::to_string),
            Some(_) => anyhow::bail!("snapshot {number}: userdata isn't an object"),
        };
        out.push(Snap {
            number,
            date,
            description: row
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
            verify,
            read_only: row
                .get("read-only")
                .and_then(|v| v.as_bool())
                .ok_or_else(|| anyhow::anyhow!("snapshot {number}: no read-only flag"))?,
        });
    }
    Ok(out)
}

/// `snapper --jsonout get-config` output → its `SUBVOLUME`.
pub fn parse_subvolume(json: &[u8]) -> anyhow::Result<PathBuf> {
    let value: serde_json::Value = serde_json::from_slice(json)?;
    value
        .get("SUBVOLUME")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("config has no SUBVOLUME"))
}

fn snapper(config: &str, args: &[&str], timeout: Duration) -> anyhow::Result<Vec<u8>> {
    let mut argv: Vec<String> = ["snapper", "--jsonout", "--utc", "-c", config]
        .iter()
        .map(|s| s.to_string())
        .collect();
    argv.extend(args.iter().map(|s| s.to_string()));
    crate::vcs::run(&argv, &[], std::path::Path::new("/"), timeout)
}

pub fn list(config: &str, timeout: Duration) -> anyhow::Result<Vec<Snap>> {
    let out = snapper(
        config,
        &[
            "list",
            "--columns",
            "number,date,description,userdata,read-only",
        ],
        timeout,
    )?;
    parse_list(&out, config)
}

pub fn subvolume(config: &str, timeout: Duration) -> anyhow::Result<PathBuf> {
    parse_subvolume(&snapper(config, &["get-config"], timeout)?)
}

/// The snapshot to compare snapshot `number` against: of the snapshots
/// this tool pruned and locked (description `pruned`, a usable `verify=`
/// tag, read-only) and older than `number`, the oldest taken at or after
/// the cutoff — so a decision change keeps being reported for the whole
/// window — or else the newest one before it. The cutoff is measured
/// from `number`'s own date (a late recovery, or a dry run against an old
/// snapshot, keeps its window), or from `now` if it isn't listed.
pub fn baseline(
    snaps: &[Snap],
    number: u64,
    since: &Since,
    now: SystemTime,
) -> anyhow::Result<Option<u64>> {
    let cutoff = match since {
        Since::At(at) => *at,
        Since::Delta(delta) => {
            let reference = snaps
                .iter()
                .find(|s| s.number == number)
                .map_or(now, |s| s.date);
            reference
                .checked_sub(*delta)
                .ok_or_else(|| anyhow::anyhow!("--since: {delta:?} is out of range"))?
        }
    };
    let usable = || {
        snaps.iter().filter(|s| {
            s.number < number
                && s.read_only
                && s.description == "pruned"
                && s.verify
                    .as_deref()
                    .is_some_and(|v| BASELINE_TAGS.contains(&v))
        })
    };
    Ok(usable()
        .filter(|s| s.date >= cutoff)
        .min_by_key(|s| (s.date, s.number))
        .or_else(|| usable().max_by_key(|s| (s.date, s.number)))
        .map(|s| s.number))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> SystemTime {
        parse_utc(text).unwrap()
    }

    #[test]
    fn utc_times_parse_only_in_the_exact_forms() {
        assert_eq!(at("2026-10-07"), at("2026-10-07 00:00:00"));
        assert_eq!(at("2026-10-07 12:30"), at("2026-10-07 12:30:00"));
        assert_eq!(
            at("1970-01-02 00:00:00"),
            SystemTime::UNIX_EPOCH + Duration::from_secs(86_400)
        );
        for bad in [
            "",
            "2026-10-7",
            "2026-10-07T12:00:00",
            "2026-10-07 12",
            "2026-13-01",
            "2026-10-07 12:00:00Z",
            "yesterday",
        ] {
            assert_eq!(parse_utc(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn since_is_a_duration_or_an_exact_utc_time() {
        assert_eq!(
            parse_since("1d").unwrap(),
            Since::Delta(Duration::from_secs(86_400))
        );
        assert_eq!(
            parse_since("6h").unwrap(),
            Since::Delta(Duration::from_secs(21_600))
        );
        assert_eq!(
            parse_since("2026-10-07").unwrap(),
            Since::At(at("2026-10-07"))
        );
        assert!(parse_since("soon").is_err());
        assert!(parse_since("-1d").is_err());
    }

    #[test]
    fn config_names_are_plain() {
        for ok in ["src", "home-1", "a_b.c"] {
            assert!(valid_config(ok), "{ok}");
        }
        for bad in ["", "-c", "--all", "a/b", "a b", "a\nb"] {
            assert!(!valid_config(bad), "{bad:?}");
        }
    }

    /// What `snapper --jsonout --utc -c src list --columns
    /// number,date,description,userdata,read-only` prints (0.13).
    const SAMPLE: &str = r#"{
      "src": [
        {"number": 0, "date": "", "description": "current", "userdata": null, "read-only": false},
        {"number": 1, "date": "2026-10-07 10:00:00", "description": "pruned",
         "userdata": {"verify": "ok"}, "read-only": true},
        {"number": 2, "date": "2026-10-07 11:00:00", "description": "timeline",
         "userdata": null, "read-only": true},
        {"number": 3, "date": "2026-10-08 09:00:00", "description": "pruned",
         "userdata": {"verify": "rules-changed", "other": "x"}, "read-only": true}
      ],
      "other": [{"number": 9}]
    }"#;

    #[test]
    fn the_list_is_parsed_from_snappers_real_shape() {
        let snaps = parse_list(SAMPLE.as_bytes(), "src").unwrap();
        assert_eq!(
            snaps.iter().map(|s| s.number).collect::<Vec<_>>(),
            [1, 2, 3]
        );
        assert_eq!(snaps[0].date, at("2026-10-07 10:00:00"));
        assert_eq!(snaps[0].verify.as_deref(), Some("ok"));
        assert_eq!(snaps[1].verify, None);
        assert_eq!(snaps[2].verify.as_deref(), Some("rules-changed"));
        assert!(snaps.iter().all(|s| s.read_only));
        // Only the requested config's key is read.
        assert!(parse_list(SAMPLE.as_bytes(), "other").is_err());
        assert!(parse_list(SAMPLE.as_bytes(), "missing").is_err());
    }

    #[test]
    fn a_malformed_list_is_an_error_not_a_partial_answer() {
        for bad in [
            "",
            "[]",
            r#"{"src": [{"number": 1, "date": "x", "read-only": true}]}"#,
            r#"{"src": [{"number": 1, "date": "2026-10-07 10:00:00"}]}"#,
            r#"{"src": [{"number": 1, "date": "2026-10-07 10:00:00", "read-only": true, "userdata": "verify=ok"}]}"#,
            r#"{"src": [{"number": "1"}]}"#,
        ] {
            assert!(parse_list(bad.as_bytes(), "src").is_err(), "{bad}");
        }
    }

    #[test]
    fn get_config_gives_the_subvolume() {
        let json = br#"{"SUBVOLUME": "/home/u/src", "FSTYPE": "btrfs"}"#;
        assert_eq!(parse_subvolume(json).unwrap(), PathBuf::from("/home/u/src"));
        assert!(parse_subvolume(br#"{"FSTYPE": "btrfs"}"#).is_err());
    }

    fn snap(number: u64, date: &str, verify: Option<&str>) -> Snap {
        Snap {
            number,
            date: at(date),
            description: "pruned".into(),
            verify: verify.map(str::to_string),
            read_only: true,
        }
    }

    #[test]
    fn the_baseline_is_the_oldest_usable_snapshot_in_the_window() {
        let snaps = vec![
            snap(1, "2026-10-06 12:00", Some("ok")),
            snap(2, "2026-10-07 13:00", Some("ok")),
            snap(3, "2026-10-08 09:00", Some("refused")),
            snap(4, "2026-10-08 12:00", Some("pending")),
        ];
        let now = at("2026-10-08 12:30");
        let day = Since::Delta(Duration::from_secs(86_400));
        // From snapshot 4's own date (12:00): cutoff 2026-10-07 12:00.
        assert_eq!(baseline(&snaps, 4, &day, now).unwrap(), Some(2));
        // A run from snapshot 3 (09:00): 2 is still inside its window.
        assert_eq!(baseline(&snaps, 3, &day, now).unwrap(), Some(2));
        // An exact time.
        let at_8th = Since::At(at("2026-10-08"));
        assert_eq!(baseline(&snaps, 4, &at_8th, now).unwrap(), Some(3));
    }

    #[test]
    fn with_nothing_in_the_window_the_newest_older_one_is_used() {
        let snaps = vec![
            snap(1, "2026-10-01 12:00", Some("ok")),
            snap(2, "2026-10-02 12:00", Some("ok")),
            snap(5, "2026-10-08 12:00", Some("pending")),
        ];
        let hour = Since::Delta(Duration::from_secs(3600));
        assert_eq!(
            baseline(&snaps, 5, &hour, at("2026-10-08 12:30")).unwrap(),
            Some(2)
        );
        assert_eq!(
            baseline(&snaps[..0], 5, &hour, at("2026-10-08 12:30")).unwrap(),
            None
        );
    }

    #[test]
    fn unusable_snapshots_are_never_a_baseline() {
        let mut manual = snap(2, "2026-10-08 10:00", Some("ok"));
        manual.description = "before upgrade".into();
        let mut writable = snap(3, "2026-10-08 10:30", Some("ok"));
        writable.read_only = false;
        let snaps = vec![
            snap(1, "2026-10-08 09:00", Some("failed")),
            manual,
            writable,
            snap(4, "2026-10-08 11:00", Some("pending")),
            snap(5, "2026-10-08 11:30", None),
            snap(7, "2026-10-08 11:40", Some("ok")), // newer than self
        ];
        let day = Since::Delta(Duration::from_secs(86_400));
        assert_eq!(
            baseline(&snaps, 6, &day, at("2026-10-08 12:00")).unwrap(),
            None
        );
    }

    #[test]
    fn an_unlisted_snapshot_measures_from_now_and_overflow_is_an_error() {
        let snaps = vec![
            snap(1, "2026-10-08 06:00", Some("ok")),
            snap(2, "2026-10-08 11:00", Some("ok")),
        ];
        let three_hours = Since::Delta(Duration::from_secs(3 * 3600));
        assert_eq!(
            baseline(&snaps, 9, &three_hours, at("2026-10-08 12:00")).unwrap(),
            Some(2)
        );
        // A huge window just reaches back to the oldest; only a real
        // overflow is an error.
        let decades = Since::Delta(Duration::from_secs(100 * 365 * 86_400));
        assert_eq!(
            baseline(&snaps, 9, &decades, at("2026-10-08 12:00")).unwrap(),
            Some(1)
        );
        assert!(
            baseline(
                &snaps,
                9,
                &Since::Delta(Duration::MAX),
                at("2026-10-08 12:00")
            )
            .is_err()
        );
    }
}
