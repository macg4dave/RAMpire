//! The controller's journal as the live views read it: its event lines and
//! their `key=value` fields, as `docs/journal.md` defines them.

use std::ffi::OsString;
use std::path::Path;

use crate::host_readings::COMMAND_TIMEOUT;
use crate::process;

/// The newest journal lines a view reads; only event lines are asked for.
const LINES: &str = "600";

/// The only keys whose values contain spaces; they end the line.
const SPACED_KEYS: [&str; 2] = ["reason", "command_result"];

const UNREADABLE: &str = "journal unreadable: run with sudo or join the systemd-journal group";

/// The unit's event lines, oldest first, or why the journal gave none: an
/// unprivileged user outside `systemd-journal` gets no lines at all, not
/// an error. Only event lines are asked for, so a burst of other output
/// (libvirt's own error lines) cannot push every event out of the window.
pub fn read_events(unit: &str, cwd: &Path) -> Result<String, String> {
    let arguments: Vec<OsString> = [
        "-u",
        unit,
        "--grep",
        " event=",
        "-n",
        LINES,
        "--no-pager",
        "-o",
        "short-iso-precise",
    ]
    .iter()
    .map(process::os)
    .collect();
    let root = process::effective_uid() == 0;
    match process::bounded_text("journalctl", &arguments, cwd, COMMAND_TIMEOUT) {
        Ok(journal) if journal.lines().any(|line| line.contains(" event=")) => {
            Ok(oldest_first(&journal))
        }
        Ok(_) if root => Ok(String::new()),
        // journalctl exits 1 when --grep matches nothing.
        Err(error) if root && error.contains("status 1") => Ok(String::new()),
        Ok(_) => Err(UNREADABLE.to_owned()),
        Err(error) if error.contains("permission") => Err(UNREADABLE.to_owned()),
        Err(error) => Err(error),
    }
}

/// The value of `name=` in an event line. Values hold no spaces, except
/// `reason` and `command_result`, which run to the other of the two or the
/// end of the line. Other keys are looked for only before those two, so
/// text inside a reason is never taken for a key.
pub fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let spaced_start = |text: &str| {
        SPACED_KEYS
            .iter()
            .filter_map(|key| text.find(&format!(" {key}=")))
            .min()
    };
    let spaced = SPACED_KEYS.contains(&name);
    let searched = if spaced {
        line
    } else {
        &line[..spaced_start(line).unwrap_or(line.len())]
    };
    let marker = format!(" {name}=");
    let start = searched.find(&marker)? + marker.len();
    let rest = &line[start..];
    let end = if spaced {
        spaced_start(rest).unwrap_or(rest.len())
    } else {
        rest.find(' ').unwrap_or(rest.len())
    };
    Some(rest[..end].trim_end())
}

/// The lines in time order. `journalctl` prints matches newest first when
/// `--grep` and `-n` go together on some systemd versions, so the order it
/// used is not relied on: microsecond timestamps sort as text, and the sort
/// is stable for any that tie.
fn oldest_first(journal: &str) -> String {
    let mut lines: Vec<&str> = journal.lines().collect();
    lines.sort_by_key(|line| line.split(' ').next().unwrap_or(""));
    lines.join("\n")
}

/// The line's time of day from its `short-iso-precise` timestamp.
pub fn time(line: &str) -> &str {
    line.get(11..19).unwrap_or("--:--:--")
}

/// At most `count` of `newest_first`, with each run of consecutive items
/// that are `same` collapsed into its newest one and the run's length.
pub fn collapse_repeats<T>(
    newest_first: impl IntoIterator<Item = T>,
    count: usize,
    same: impl Fn(&T, &T) -> bool,
) -> Vec<(T, usize)> {
    let mut collapsed: Vec<(T, usize)> = Vec::new();
    for item in newest_first {
        if let Some((last, repeats)) = collapsed.last_mut() {
            if same(last, &item) {
                *repeats += 1;
                continue;
            }
        }
        if collapsed.len() == count {
            break;
        }
        collapsed.push((item, 1));
    }
    collapsed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_come_back_oldest_first_whatever_order_journalctl_used() {
        let newest_first = "2026-11-14T20:17:08.000002+00:00 h rampire[1]: event=b\n\
                            2026-11-14T20:17:08.000001+00:00 h rampire[1]: event=a\n\
                            2026-11-14T12:22:36.500000+00:00 h rampire[1]: event=old";
        let sorted = oldest_first(newest_first);
        let events: Vec<_> = sorted
            .lines()
            .map(|line| line.rsplit('=').next().unwrap())
            .collect();
        assert_eq!(events, ["old", "a", "b"]);
        assert_eq!(oldest_first(&sorted), sorted);
        assert_eq!(time(sorted.lines().next().unwrap()), "12:22:36");
    }

    const LINE: &str = "2026-11-14T16:12:59+00:00 h rampire[1]: rampire: vm=g event=resize_settled_short operation_id=7 kept_bytes=5 reason=guest kept vm=other memory command_result=Partial at 5 bytes";

    #[test]
    fn fields_follow_the_documented_line_format() {
        assert_eq!(field(LINE, "vm"), Some("g"));
        assert_eq!(field(LINE, "event"), Some("resize_settled_short"));
        assert_eq!(field(LINE, "kept_bytes"), Some("5"));
        assert_eq!(field(LINE, "reason"), Some("guest kept vm=other memory"));
        assert_eq!(field(LINE, "command_result"), Some("Partial at 5 bytes"));
        // A key named only inside a reason is not one.
        assert_eq!(field(LINE, "memory"), None);
        assert_eq!(field(LINE, "operation"), None);
        assert_eq!(field(LINE, "id"), None);
        assert_eq!(time(LINE), "16:12:59");
        assert_eq!(time("short"), "--:--:--");
    }

    #[test]
    fn consecutive_repeats_collapse_into_the_newest() {
        let collapsed = collapse_repeats(["a1", "a2", "b", "a3", "c"], 3, |left, right| {
            left[..1] == right[..1]
        });
        assert_eq!(collapsed, [("a1", 2), ("b", 1), ("a3", 1)]);
        assert!(collapse_repeats(["a"], 0, |_, _| false).is_empty());
    }
}
