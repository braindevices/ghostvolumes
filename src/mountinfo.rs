//! Parses `/proc/self/mountinfo` (format: `man 5 proc`) to find BTRFS
//! mountpoints, for `scan`'s unprivileged detection pass (§3). Pure
//! text-in/data-out — no filesystem access here, so fully testable
//! without a real mount table.
//!
//! Line shape: `<id> <parent> <major:minor> <root> <mountpoint>
//! <options> <optional fields...> - <fstype> <source> <super options>`.
//! The `-` separator is what marks the end of the variable-length
//! optional-fields section.

/// Undoes mountinfo's octal escaping (`\040` for space, etc.) — mount
/// paths containing whitespace or backslashes are escaped this way.
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = String::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && let Ok(octal) = std::str::from_utf8(&bytes[i + 1..i + 4])
            && let Ok(value) = u8::from_str_radix(octal, 8)
        {
            out.push(value as char);
            i += 4;
            continue;
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

/// Mountpoints whose filesystem type is `btrfs`.
pub fn btrfs_mountpoints(mountinfo_text: &str) -> Vec<String> {
    mountinfo_text
        .lines()
        .filter_map(parse_line)
        .filter(|(fstype, _)| fstype == "btrfs")
        .map(|(_, mountpoint)| mountpoint)
        .collect()
}

/// Returns `(fstype, mountpoint)` for one mountinfo line, or `None` if
/// it doesn't match the expected shape (tolerate garbage rather than
/// erroring — this is best-effort discovery, not a hard requirement).
fn parse_line(line: &str) -> Option<(String, String)> {
    let (before_dash, after_dash) = line.split_once(" - ")?;
    let mountpoint = before_dash.split_whitespace().nth(4)?;
    let fstype = after_dash.split_whitespace().next()?;
    Some((fstype.to_string(), unescape(mountpoint)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_btrfs_mountpoints() {
        let text = "\
36 35 0:31 / / rw,relatime shared:1 - btrfs /dev/sda2 rw,ssd,space_cache
37 35 0:32 / /home rw,relatime shared:2 - ext4 /dev/sda3 rw
38 35 0:33 / /dbs rw,relatime shared:3 - btrfs /dev/sda4 rw,ssd
39 35 0:34 / /proc rw,relatime shared:4 - proc proc rw
";
        assert_eq!(btrfs_mountpoints(text), vec!["/", "/dbs"]);
    }

    #[test]
    fn no_btrfs_mounts_yields_empty() {
        let text = "37 35 0:32 / /home rw,relatime shared:2 - ext4 /dev/sda3 rw\n";
        assert!(btrfs_mountpoints(text).is_empty());
    }

    #[test]
    fn empty_input_yields_empty() {
        assert!(btrfs_mountpoints("").is_empty());
    }

    #[test]
    fn malformed_lines_are_skipped_not_erroring() {
        let text = "\
this is not a valid mountinfo line at all
36 35 0:31 / / rw,relatime shared:1 - btrfs /dev/sda2 rw,ssd
";
        assert_eq!(btrfs_mountpoints(text), vec!["/"]);
    }

    #[test]
    fn variable_length_optional_fields_are_handled() {
        // Zero optional fields (just "-" immediately after options)
        // and multiple optional fields both need to parse correctly.
        let text = "\
36 35 0:31 / /a rw,relatime - btrfs /dev/sda2 rw
37 35 0:32 / /b rw,relatime shared:1 master:2 propagate_from:3 - btrfs /dev/sda3 rw
";
        assert_eq!(btrfs_mountpoints(text), vec!["/a", "/b"]);
    }

    #[test]
    fn unescapes_octal_sequences_in_mountpoint() {
        // A mountpoint containing a space is escaped as \040 in
        // /proc/self/mountinfo.
        let text = "36 35 0:31 / /mnt/my\\040drive rw,relatime - btrfs /dev/sda2 rw\n";
        assert_eq!(btrfs_mountpoints(text), vec!["/mnt/my drive"]);
    }
}
