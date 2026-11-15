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

/// The `journalctl --grep` pattern for event lines.
const EVENT_PATTERN: &str = "(^|\\s)event=";

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
        // A controller line starts with its event; an operator's has
        // `rampire:` before it.
        EVENT_PATTERN,
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
        Err(error) if root && process::exited_one(&error) => Ok(String::new()),
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

/// `journalctl` arguments for `rampire logs`: the controller unit's lines,
/// systemd's about the unit, and the changes people made (logged under the
/// identifier `rampire`), optionally since a time and only about one VM.
pub fn log_arguments(since: Option<&str>, vm: Option<&str>) -> Vec<String> {
    let unit = crate::systemd_unit::CONTROLLER_UNIT;
    let mut arguments: Vec<String> = vec![
        "--no-pager".to_owned(),
        format!("_SYSTEMD_UNIT={unit}"),
        "+".to_owned(),
        format!("UNIT={unit}"),
        "+".to_owned(),
        format!("SYSLOG_IDENTIFIER={}", crate::audit::IDENTIFIER),
    ];
    if let Some(since) = since {
        arguments.extend(["--since".to_owned(), since.to_owned()]);
    }
    if let Some(vm) = vm {
        arguments.extend(["--grep".to_owned(), vm_pattern(vm)]);
    }
    arguments
}

/// A `journalctl --grep` pattern for the lines naming `vm` as their VM.
pub fn vm_pattern(vm: &str) -> String {
    let mut escaped = String::new();
    for character in vm.chars() {
        if !character.is_ascii_alphanumeric() {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    format!("(^|\\s)vm={escaped}(\\s|$)")
}

/// The newest `lines` of `rampire logs` (optionally about one VM), oldest
/// first, for the TUI's logs screen.
pub fn read_log(vm: Option<&str>, lines: usize, cwd: &Path) -> Result<String, String> {
    let mut arguments: Vec<OsString> = log_arguments(None, vm).iter().map(process::os).collect();
    for argument in ["-n", &lines.to_string(), "-o", "short-iso"] {
        arguments.push(process::os(argument));
    }
    match process::bounded_text("journalctl", &arguments, cwd, COMMAND_TIMEOUT) {
        Ok(text) => Ok(text),
        // journalctl exits 1 when --grep matches nothing.
        Err(error) if process::exited_one(&error) => Ok(String::new()),
        Err(error) if error.contains("permission") => Err(UNREADABLE.to_owned()),
        Err(error) => Err(error),
    }
}

/// What the controller last said about `vm`'s control channel in the last
/// hour: `connected`, or `unavailable` with its reason. `None` when it said
/// nothing, or the journal can't be read.
pub fn channel_event(vm: &str, cwd: &Path) -> Option<String> {
    let unit = crate::systemd_unit::CONTROLLER_UNIT;
    let arguments: Vec<OsString> = [
        "--no-pager".to_owned(),
        format!("_SYSTEMD_UNIT={unit}"),
        "--since".to_owned(),
        "-1h".to_owned(),
        "-o".to_owned(),
        "cat".to_owned(),
        "--grep".to_owned(),
        format!("{}.*event=control_channel_", vm_pattern(vm)),
    ]
    .iter()
    .map(process::os)
    .collect();
    let text = process::bounded_text("journalctl", &arguments, cwd, COMMAND_TIMEOUT).ok()?;
    channel_event_in(&text)
}

/// The newest control-channel event among `text`'s lines, oldest first.
fn channel_event_in(text: &str) -> Option<String> {
    let line = text
        .lines()
        .rev()
        .find(|line| line.contains("event=control_channel_"))?;
    let event = field(line, "event")?.trim_start_matches("control_channel_");
    Some(match line.split_once(" reason=") {
        Some((_, reason)) => format!("{event}: {}", reason.trim()),
        None => event.to_owned(),
    })
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
    fn the_newest_channel_event_says_why_with_its_reason() {
        let text = "rampire: vm=tv101 event=control_channel_connected\n\
            rampire: vm=tv101 event=control_channel_unavailable reason=connect: Permission denied (os error 13)\n";
        assert_eq!(
            channel_event_in(text).as_deref(),
            Some("unavailable: connect: Permission denied (os error 13)")
        );
        assert_eq!(
            channel_event_in("rampire: vm=tv101 event=control_channel_connected").as_deref(),
            Some("connected")
        );
        assert_eq!(channel_event_in(""), None);
    }

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
    fn logs_name_the_unit_and_peoples_changes_and_one_vm_exactly() {
        let arguments = log_arguments(Some("1 hour ago"), Some("web.1"));
        assert_eq!(
            arguments,
            [
                "--no-pager",
                "_SYSTEMD_UNIT=rampire.service",
                "+",
                "UNIT=rampire.service",
                "+",
                "SYSLOG_IDENTIFIER=rampire",
                "--since",
                "1 hour ago",
                "--grep",
                r"(^|\s)vm=web\.1(\s|$)",
            ]
        );
        assert_eq!(log_arguments(None, None).len(), 6);
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
