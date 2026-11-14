//! What a person can do from the TUI. Each action is one of the commands'
//! own flows, run with the TUI's dialogs answering it, and names the command
//! line that does the same, so a person learns the commands and can script
//! what they did.

use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use super::jobs::{self, ChannelUi, Msg, ReaderControl};
use crate::context::Context;
use crate::flows::{apply, controller, lifecycle};
use crate::ui::Ui;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `vm add VM --fix-xml`: a new VM, an unfinished one or a parked one.
    Add(String),
    /// `vm remove VM`, parking its settings, or forgetting them.
    Remove {
        vm: String,
        forget: bool,
    },
    /// `vm attest VM`, optionally restarting it first.
    Attest {
        vm: String,
        restart: bool,
    },
    /// `set TARGET KEY=VALUE...`.
    Set {
        target: String,
        changes: Vec<String>,
    },
    Apply,
    /// `recover VM REASON`; the reason is asked.
    Recover(String),
    Init,
}

impl Action {
    /// The words after `rampire` that do the same.
    pub fn words(&self) -> Vec<String> {
        let owned = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        match self {
            Self::Add(vm) => owned(&["vm", "add", vm, "--fix-xml"]),
            Self::Remove { vm, forget: false } => owned(&["vm", "remove", vm]),
            Self::Remove { vm, forget: true } => owned(&["vm", "remove", vm, "--forget"]),
            Self::Attest { vm, restart: false } => owned(&["vm", "attest", vm]),
            Self::Attest { vm, restart: true } => owned(&["vm", "attest", vm, "--restart"]),
            Self::Set { target, changes } => {
                let mut words = owned(&["set", target]);
                words.extend(changes.iter().cloned());
                words
            }
            Self::Apply => owned(&["apply"]),
            Self::Recover(vm) => owned(&["recover", vm, "REASON"]),
            Self::Init => owned(&["init"]),
        }
    }

    /// The command line, as a person would type it.
    pub fn command_line(&self, context: &Context) -> String {
        let words = self.words();
        let words: Vec<&str> = words.iter().map(String::as_str).collect();
        context.command_line(&words)
    }

    /// The VM this action is about, if it is about one.
    pub fn vm(&self) -> Option<&str> {
        match self {
            Self::Add(vm)
            | Self::Remove { vm, .. }
            | Self::Attest { vm, .. }
            | Self::Recover(vm) => Some(vm),
            Self::Set { target, .. } if target != crate::set::GLOBAL => Some(target),
            _ => None,
        }
    }

    /// Run it on its own thread. The reader reloads the configuration when
    /// it ends, whatever it changed.
    pub fn spawn(&self, context: &Context, control: &ReaderControl, tx: Sender<Msg>) {
        let action = self.clone();
        let context = context.clone();
        let reload = control.reload.clone();
        jobs::spawn_flow(tx, move |ui| {
            let result = action.run(&context, ui);
            reload.store(true, Ordering::Release);
            result
        });
    }

    fn run(&self, context: &Context, ui: &mut ChannelUi) -> Result<String, String> {
        match self {
            Self::Add(vm) => {
                let changed = lifecycle::vm_add(context, ui, &lifecycle::AddOptions::fixing(vm))?;
                if changed {
                    first_publication(context, ui, vm, Duration::from_secs(180));
                }
                Ok(format!("{vm} added"))
            }
            Self::Remove { vm, forget } => {
                lifecycle::vm_remove(context, ui, vm, *forget)?;
                Ok("done".to_owned())
            }
            Self::Attest { vm, restart } => {
                lifecycle::vm_attest(context, ui, Some(vm), false, *restart, false)?;
                Ok("done".to_owned())
            }
            Self::Set { target, changes } => {
                let changes = changes
                    .iter()
                    .map(|change| crate::set::parse_assignment(change))
                    .collect::<Result<Vec<_>, _>>()?;
                lifecycle::set(context, ui, target, &changes, false)?;
                Ok("done".to_owned())
            }
            Self::Apply => {
                apply::apply(context, ui)?;
                Ok("done".to_owned())
            }
            Self::Recover(vm) => {
                let reason = ui.text(&format!(
                    "Why is it safe to resume resizing {vm}? (recorded in its state)"
                ))?;
                if reason.trim().is_empty() {
                    return Err("a recovery needs a reason; nothing was changed".to_owned());
                }
                ui.say(&format!(
                    "same as: {}",
                    context.command_line(&["recover", vm, reason.trim()])
                ));
                controller::recover(context, ui, vm, reason.trim())?;
                Ok("done".to_owned())
            }
            Self::Init => {
                controller::init(context, ui)?;
                Ok("done".to_owned())
            }
        }
    }
}

/// After a VM is added, wait for its guest's first publication to reach the
/// controller and say so; or say how long it waited.
fn first_publication(context: &Context, ui: &mut dyn Ui, vm: &str, limit: Duration) {
    ui.say(&format!("waiting for {vm}'s guest service to publish..."));
    let started = Instant::now();
    while started.elapsed() < limit {
        if let Ok(status) = crate::inventory::read_status(context, vm) {
            if status.guest_input.observed_unix_millis.is_some() {
                ui.say(&format!(
                    "{vm}'s guest published: {}",
                    crate::reports::status_line(&status)
                ));
                return;
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    ui.say(&format!(
        "{vm}'s guest hasn't published after {}s: check rampire-guest in the VM (systemctl status rampire-guest), then rampire doctor {vm}",
        limit.as_secs()
    ));
}

/// The action a doctor's fix names, when it is a whole `rampire` command
/// the TUI can run: no placeholder to fill in, other than a recovery's
/// reason, which is asked.
pub fn from_fix(fix: &str) -> Option<Action> {
    // Two commands in turn are for a person to run.
    if fix.matches("rampire ").count() > 1 && fix.contains("then") {
        return None;
    }
    let start = fix.find("rampire ")?;
    let command = fix[start + "rampire ".len()..]
        .split([';', ',', '('])
        .next()?
        .trim()
        .trim_end_matches('.');
    let words: Vec<&str> = command.split_whitespace().collect();
    let placeholder = |word: &str| {
        word.len() > 1
            && word
                .chars()
                .all(|character| character.is_ascii_uppercase() || "_=".contains(character))
    };
    match words.as_slice() {
        ["apply"] => Some(Action::Apply),
        ["init", ..] => Some(Action::Init),
        ["vm", "add", vm, rest @ ..] if rest.iter().all(|word| *word == "--fix-xml") => {
            Some(Action::Add((*vm).to_owned()))
        }
        ["vm", "attest", vm] => Some(Action::Attest {
            vm: (*vm).to_owned(),
            restart: false,
        }),
        ["vm", "attest", vm, "--restart"] => Some(Action::Attest {
            vm: (*vm).to_owned(),
            restart: true,
        }),
        ["vm", "remove", vm] => Some(Action::Remove {
            vm: (*vm).to_owned(),
            forget: false,
        }),
        ["vm", "remove", vm, "--forget"] => Some(Action::Remove {
            vm: (*vm).to_owned(),
            forget: true,
        }),
        ["recover", vm, ..] => Some(Action::Recover((*vm).to_owned())),
        ["set", target, changes @ ..]
            if !changes.is_empty()
                && changes.iter().all(|change| {
                    change
                        .split_once('=')
                        .is_some_and(|(_, value)| !placeholder(value) && !value.is_empty())
                }) =>
        {
            Some(Action::Set {
                target: (*target).to_owned(),
                changes: changes.iter().map(|change| (*change).to_owned()).collect(),
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_names_its_command_line() {
        let context = Context {
            config: None,
            connect: None,
            build: String::new(),
            via: "tui",
        };
        assert_eq!(
            Action::Add("webserver".to_owned()).command_line(&context),
            "rampire vm add webserver --fix-xml"
        );
        assert_eq!(
            Action::Remove {
                vm: "web".to_owned(),
                forget: true
            }
            .command_line(&context),
            "rampire vm remove web --forget"
        );
        assert_eq!(
            Action::Set {
                target: "web".to_owned(),
                changes: vec!["max_ram=12GB".to_owned(), "priority=150".to_owned()]
            }
            .command_line(&context),
            "rampire set web max_ram=12GB priority=150"
        );
    }

    #[test]
    fn a_fix_that_is_a_whole_command_can_be_run() {
        assert_eq!(
            from_fix("if the change was intended, rampire vm attest web"),
            Some(Action::Attest {
                vm: "web".to_owned(),
                restart: false
            })
        );
        assert_eq!(
            from_fix("rampire vm attest web --restart"),
            Some(Action::Attest {
                vm: "web".to_owned(),
                restart: true
            })
        );
        assert_eq!(
            from_fix("rampire init on a new host, or rampire apply"),
            Some(Action::Init)
        );
        assert_eq!(
            from_fix("once the guest is checked, rampire recover web \"WHY IT IS SAFE TO RESUME\""),
            Some(Action::Recover("web".to_owned()))
        );
        assert_eq!(
            from_fix("rampire vm add web --fix-xml"),
            Some(Action::Add("web".to_owned()))
        );
        // A placeholder to fill in, or no rampire command at all, is not run.
        assert_eq!(
            from_fix("raise max_ram: rampire set web max_ram=SIZE"),
            None
        );
        assert_eq!(from_fix("boot the guest with psi=1"), None);
        assert_eq!(from_fix("rampire doctor web"), None);
        assert_eq!(
            from_fix("rampire vm remove web, then rampire vm add web --fix-xml"),
            None
        );
    }
}
