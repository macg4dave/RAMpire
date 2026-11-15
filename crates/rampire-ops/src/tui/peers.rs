//! Changes other people make while the TUI is open, from a command or
//! another TUI, read from the journal's `event=operator_change` lines
//! (docs/journal.md), so two people on one host see each other's work.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::process;

/// One change someone else made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerChange {
    pub user: String,
    pub via: String,
    pub action: String,
    pub vm: Option<String>,
    /// Its other `key=value` fields, as written.
    pub rest: String,
}

impl PeerChange {
    /// In a line: `set web1 priority=150 (via cli)`.
    pub fn text(&self) -> String {
        let mut words = vec![self.action.clone()];
        words.extend(self.vm.clone());
        if !self.rest.is_empty() {
            words.push(self.rest.clone());
        }
        format!("{} (via {})", words.join(" "), self.via)
    }
}

/// The journal's changes since the TUI opened, each told once.
pub struct Watch {
    since: u64,
    seen: HashSet<String>,
    me: String,
    /// Off after the journal couldn't be read: the logs screen says why.
    off: bool,
}

/// How many of the newest changes are read each time.
const NEWEST: &str = "200";
const TIMEOUT: Duration = Duration::from_secs(5);

impl Watch {
    pub fn new() -> Self {
        let since = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |age| age.as_secs());
        Self {
            since,
            seen: HashSet::new(),
            me: crate::audit::person(),
            off: false,
        }
    }

    /// The changes made by others since the last poll.
    pub fn poll(&mut self) -> Vec<PeerChange> {
        if self.off {
            return Vec::new();
        }
        let arguments: Vec<std::ffi::OsString> = [
            "-t",
            crate::audit::IDENTIFIER,
            "--grep",
            "event=operator_change",
            "--since",
            &format!("@{}", self.since),
            "-n",
            NEWEST,
            "-o",
            "short-iso-precise",
            "-q",
            "--no-pager",
        ]
        .iter()
        .map(process::os)
        .collect();
        let text = match process::bounded_text("journalctl", &arguments, Path::new("/"), TIMEOUT) {
            Ok(text) => text,
            // journalctl exits 1 when --grep matches nothing.
            Err(error) if error.contains("status 1") => String::new(),
            Err(_) => {
                self.off = true;
                return Vec::new();
            }
        };
        self.news(&text)
    }

    /// The lines of `text` not seen before, made by someone else.
    fn news(&mut self, text: &str) -> Vec<PeerChange> {
        text.lines()
            .filter(|line| self.seen.insert((*line).to_owned()))
            .filter_map(parse)
            .filter(|change| change.user != self.me)
            .collect()
    }
}

impl Default for Watch {
    fn default() -> Self {
        Self::new()
    }
}

/// A change from its journal line.
pub fn parse(line: &str) -> Option<PeerChange> {
    let message = line.split_once(" rampire: ")?.1;
    let field = |name: &str| crate::journal::field(line, name).map(str::to_owned);
    if field("event").as_deref() != Some("operator_change") {
        return None;
    }
    // The plain `key=value` words, then a reason, which may hold spaces.
    let known = ["vm", "event", "action", "user", "via", "reason"];
    let plain = message.split(" reason=").next().unwrap_or(message);
    let mut rest: Vec<String> = plain
        .split_whitespace()
        .filter(|word| {
            word.split_once('=')
                .is_some_and(|(key, _)| !known.contains(&key))
        })
        .map(str::to_owned)
        .collect();
    rest.extend(field("reason").map(|reason| format!("reason={reason}")));
    Some(PeerChange {
        user: field("user")?,
        via: field("via").unwrap_or_else(|| "-".to_owned()),
        action: field("action")?,
        vm: field("vm"),
        rest: rest.join(" "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = "2026-11-15T10:00:00.123456+0000 pc101 rampire[77]: rampire: vm=web event=operator_change action=set user=alice via=cli priority=150";

    #[test]
    fn a_change_is_read_from_its_journal_line() {
        let change = parse(LINE).expect("a change");
        assert_eq!(change.user, "alice");
        assert_eq!(change.vm.as_deref(), Some("web"));
        assert_eq!(change.text(), "set web priority=150 (via cli)");
        let recover = "2026-11-15T10:00:00 pc101 rampire[7]: rampire: vm=web event=operator_change action=recover user=bob via=tui reason=checked the guest by hand";
        assert_eq!(
            parse(recover).unwrap().text(),
            "recover web reason=checked the guest by hand (via tui)"
        );
        assert_eq!(
            parse("2026-11-15T10:00:00 pc101 rampire[1]: rampire: vm=web event=grant"),
            None
        );
    }

    #[test]
    fn each_change_by_someone_else_is_told_once() {
        let mut watch = Watch {
            since: 0,
            seen: HashSet::new(),
            me: "dave".to_owned(),
            off: false,
        };
        let mine = LINE.replace("user=alice", "user=dave");
        let text = format!("{LINE}\n{mine}\n");
        assert_eq!(watch.news(&text).len(), 1);
        assert!(watch.news(&text).is_empty());
    }
}
