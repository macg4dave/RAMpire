//! Every change a person makes, from a command or from the TUI, is logged to
//! the journal as one `event=operator_change` line, with who made it and
//! from where, beside the controller's own events (docs/journal.md).

use std::path::Path;
use std::time::Duration;

use crate::context::Context;
use crate::process;

/// The journal identifier of a person's changes: the controller's own, so
/// `rampire logs` and `journalctl -t rampire` show both.
pub const IDENTIFIER: &str = "rampire";

/// The person behind the command: the one who ran sudo, else the account.
pub fn person() -> String {
    ["SUDO_USER", "USER", "LOGNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "root".to_owned())
}

/// The journal line for a change: `action` on `vm` (or the host), with
/// more `key=value` pairs. Values have their spaces replaced, so every
/// key is read as docs/journal.md says, except `reason` which ends a line.
pub fn line(context: &Context, action: &str, vm: Option<&str>, fields: &[(&str, &str)]) -> String {
    let mut words = vec!["rampire:".to_owned()];
    if let Some(vm) = vm {
        words.push(format!("vm={vm}"));
    }
    words.push("event=operator_change".to_owned());
    words.push(format!("action={action}"));
    words.push(format!("user={}", value(&person())));
    words.push(format!("via={}", context.via));
    let mut reason = None;
    for (key, text) in fields {
        if *key == "reason" {
            reason = Some(text.to_string());
        } else {
            words.push(format!("{key}={}", value(text)));
        }
    }
    if let Some(reason) = reason {
        words.push(format!("reason={reason}"));
    }
    words.join(" ")
}

fn value(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|character| {
            if character.is_whitespace() {
                '_'
            } else {
                character
            }
        })
        .collect();
    if cleaned.is_empty() {
        "-".to_owned()
    } else {
        cleaned
    }
}

/// Log a change. A journal that can't be written doesn't stop the change,
/// which has already been made; the failure is reported on stderr.
pub fn record(context: &Context, action: &str, vm: Option<&str>, fields: &[(&str, &str)]) {
    let text = line(context, action, vm, fields);
    let logged = process::bounded_text(
        "logger",
        &[
            process::os("--tag"),
            process::os(IDENTIFIER),
            process::os("--"),
            process::os(&text),
        ],
        Path::new("/"),
        Duration::from_secs(10),
    );
    if let Err(error) = logged {
        eprintln!("rampire: warning: could not log the change to the journal: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_names_the_person_where_it_came_from_and_ends_with_its_reason() {
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "tui",
        };
        std::env::set_var("SUDO_USER", "alice");
        let text = line(
            &context,
            "recover",
            Some("web"),
            &[("reason", "checked the guest by hand"), ("to", "/a b")],
        );
        assert_eq!(
            text,
            "rampire: vm=web event=operator_change action=recover user=alice via=tui to=/a_b reason=checked the guest by hand"
        );
        assert_eq!(crate::journal::field(&text, "via"), Some("tui"));
        assert_eq!(
            crate::journal::field(&text, "reason"),
            Some("checked the guest by hand")
        );
    }
}
