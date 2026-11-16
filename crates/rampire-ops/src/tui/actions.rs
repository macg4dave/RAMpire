//! What a person can do from the TUI. Each action is one of the commands'
//! own flows, run with the TUI's dialogs answering it, and names the command
//! line that does the same, so a person learns the commands and can script
//! what they did.

use super::jobs::{self, ChannelUi, Ended, Msg, ReaderControl, TaskId};
use crate::context::Context;
use crate::flows::{apply, controller, lifecycle, Outcome};
use crate::ui::Ui;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `vm add VM`, with the settings the form changed as `KEY=VALUE`:
    /// `priority`, `min_ram`, `max_ram` and `guest_login`.
    Add {
        vm: String,
        changes: Vec<String>,
    },
    /// `vm remove VM`.
    Remove(String),
    /// `vm restart VM`, so its saved definition takes effect.
    Restart(String),
    /// `vm upgrade VM`: the guest package of this host's build.
    Upgrade(String),
    /// `set TARGET KEY=VALUE...`.
    Set {
        target: String,
        changes: Vec<String>,
    },
    Apply,
    /// `diff`: the draft against what runs, in the task's output.
    Diff,
    /// `rollback`: back to the applied copy before, shown and asked.
    Rollback,
    /// `recover VM REASON`; the reason is asked.
    Recover(String),
    /// `init`, with `--host-reserve SIZE` when it was chosen in the setup
    /// form; else it is asked, filled in with the recommended one.
    Init {
        host_reserve: Option<String>,
    },
}

/// The `vm add` option for a setting the add form names.
fn add_flag(key: &str) -> Option<&'static str> {
    Some(match key {
        "priority" => "--priority",
        "min_ram" => "--min-ram",
        "max_ram" => "--max-ram",
        _ => return None,
    })
}

impl Action {
    /// `vm add VM` with every default.
    pub fn add(vm: &str) -> Self {
        Self::Add {
            vm: vm.to_owned(),
            changes: Vec::new(),
        }
    }

    /// The words after `rampire` that do the same.
    pub fn words(&self) -> Vec<String> {
        let owned = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect();
        match self {
            Self::Add { vm, changes } => {
                let mut words: Vec<String> = owned(&["vm", "add", vm]);
                for (key, value) in changes.iter().filter_map(|change| change.split_once('=')) {
                    if let Some(flag) = add_flag(key) {
                        words.push(flag.to_owned());
                        words.push(value.to_owned());
                    }
                }
                words
            }
            Self::Remove(vm) => owned(&["vm", "remove", vm]),
            Self::Restart(vm) => owned(&["vm", "restart", vm]),
            Self::Upgrade(vm) => owned(&["vm", "upgrade", vm]),
            Self::Set { target, changes } => {
                let mut words = owned(&["set", target]);
                words.extend(changes.iter().cloned());
                words
            }
            Self::Apply => owned(&["apply"]),
            Self::Diff => owned(&["diff"]),
            Self::Rollback => owned(&["rollback"]),
            Self::Recover(vm) => owned(&["recover", vm, "REASON"]),
            Self::Init {
                host_reserve: Some(reserve),
            } => owned(&["init", "--host-reserve", reserve]),
            Self::Init { host_reserve: None } => owned(&["init"]),
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
            Self::Add { vm, .. }
            | Self::Remove(vm)
            | Self::Restart(vm)
            | Self::Upgrade(vm)
            | Self::Recover(vm) => Some(vm),
            Self::Set { target, .. } if target != crate::set::GLOBAL => Some(target),
            _ => None,
        }
    }

    /// Run it on its own thread as task `id`. The reader reloads the
    /// configuration when it ends, whatever it changed.
    pub fn spawn(
        &self,
        context: &Context,
        control: &ReaderControl,
        tx: Sender<Msg>,
        id: TaskId,
        cancel: crate::ui::Cancel,
    ) {
        let action = self.clone();
        let context = context.clone();
        let reload = control.reload.clone();
        jobs::spawn_flow(tx, id, cancel, move |ui| {
            let result = action.run(&context, ui);
            reload.store(true, Ordering::Release);
            result
        });
    }

    fn run(&self, context: &Context, ui: &mut ChannelUi) -> Result<Ended, String> {
        let outcome = match self {
            Self::Add { vm, changes } => {
                lifecycle::vm_add(context, ui, &add_options(vm, changes)?)?
            }
            Self::Remove(vm) => lifecycle::vm_remove(context, ui, vm)?,
            Self::Restart(vm) => lifecycle::vm_restart(context, ui, vm)?,
            Self::Upgrade(vm) => lifecycle::vm_upgrade(context, ui, vm, None)?,
            Self::Set { target, changes } => {
                let changes = changes
                    .iter()
                    .map(|change| crate::set::parse_assignment(change))
                    .collect::<Result<Vec<_>, _>>()?;
                lifecycle::set(context, ui, target, &changes, false)?
            }
            Self::Apply => apply::apply(context, ui)?,
            Self::Diff => {
                apply::diff(context, ui)?;
                Outcome::Done
            }
            Self::Rollback => apply::rollback(context, ui)?,
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
                controller::recover(context, ui, vm, reason.trim())?
            }
            Self::Init { host_reserve } => controller::init(context, ui, host_reserve.as_deref())?,
        };
        Ok(self.ended(outcome))
    }

    /// What the TUI says about how the flow ended: what was done in this
    /// action's words, or the flow's own account of what wasn't.
    pub fn ended(&self, outcome: Outcome) -> Ended {
        match outcome {
            Outcome::Done => Ended::Done(self.done_text()),
            Outcome::AlreadySo(text) => Ended::Done(text),
            Outcome::Declined(text) => {
                Ended::NotDone(format!("{}: the answer was No; {text}", self.subject()))
            }
            Outcome::Partly(text) => Ended::NotDone(text),
        }
    }

    /// What this action does, as a noun phrase for its notices.
    fn subject(&self) -> String {
        match self {
            Self::Add { vm, .. } => format!("{vm} is not added"),
            Self::Remove(vm) => format!("RAMpire is not removed from {vm}"),
            Self::Restart(vm) => format!("{vm} is not restarted"),
            Self::Upgrade(vm) => format!("{vm}'s guest service is not upgraded"),
            Self::Set { target, .. } if target == crate::set::GLOBAL => {
                "the global settings are not applied".to_owned()
            }
            Self::Set { target, .. } => format!("{target}'s settings are not applied"),
            Self::Apply => "the draft is not applied".to_owned(),
            Self::Diff => "the difference is not shown".to_owned(),
            Self::Rollback => "nothing is rolled back".to_owned(),
            Self::Recover(vm) => format!("{vm} is not recovered"),
            Self::Init { .. } => "the host is not set up".to_owned(),
        }
    }

    /// What was done, when the flow did it all.
    fn done_text(&self) -> String {
        match self {
            Self::Add { vm, .. } => format!("{vm} is added and its guest service reports in"),
            Self::Remove(vm) => format!("RAMpire is removed from {vm}"),
            Self::Restart(vm) => format!("{vm} restarted onto its saved definition"),
            Self::Upgrade(vm) => format!("{vm} runs this host's guest service build"),
            Self::Set { target, .. } if target == crate::set::GLOBAL => {
                "the global settings are saved and applied".to_owned()
            }
            Self::Set { target, .. } => format!("{target}'s settings are saved and applied"),
            Self::Apply => "the draft is applied".to_owned(),
            Self::Diff => "the difference is in this task's output".to_owned(),
            Self::Rollback => "the controller runs the applied copy before".to_owned(),
            Self::Recover(vm) => format!("{vm} resumes resizing"),
            Self::Init { .. } => "the host is set up and the controller runs".to_owned(),
        }
    }
}

/// `vm add`'s options from the add form's changes.
fn add_options(vm: &str, changes: &[String]) -> Result<lifecycle::AddOptions, String> {
    let mut options = lifecycle::AddOptions::new(vm);
    for change in changes {
        let (key, value) = change
            .split_once('=')
            .ok_or_else(|| format!("{change} is not KEY=VALUE"))?;
        let value = value.trim().to_owned();
        match key {
            "priority" => {
                options.priority = value
                    .parse()
                    .map_err(|_| format!("priority {value} is not a whole number"))?;
            }
            "min_ram" => options.min_ram = Some(value),
            "max_ram" => options.max_ram = Some(value),
            _ => return Err(format!("vm add has no setting {key}")),
        }
    }
    Ok(options)
}

/// The action a doctor's finding runs, from its command's words after
/// `rampire` ([`crate::doctor::Finding::run`]): a recovery's reason is
/// asked; a command the TUI has no action for is none.
pub fn from_words(words: &[String]) -> Option<Action> {
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["apply"] => Some(Action::Apply),
        ["init", ..] => Some(Action::Init { host_reserve: None }),
        ["vm", "add", vm] => Some(Action::add(vm)),
        ["vm", "restart", vm] => Some(Action::Restart((*vm).to_owned())),
        ["vm", "upgrade", vm] => Some(Action::Upgrade((*vm).to_owned())),
        ["vm", "remove", vm] => Some(Action::Remove((*vm).to_owned())),
        ["recover", vm] => Some(Action::Recover((*vm).to_owned())),
        ["set", target, changes @ ..] if !changes.is_empty() => Some(Action::Set {
            target: (*target).to_owned(),
            changes: changes.iter().map(|change| (*change).to_owned()).collect(),
        }),
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
            Action::add("webserver").command_line(&context),
            "rampire vm add webserver"
        );
        assert_eq!(
            Action::Add {
                vm: "web".to_owned(),
                changes: vec!["max_ram=12GB".to_owned(), "priority=150".to_owned()]
            }
            .command_line(&context),
            "rampire vm add web --max-ram 12GB --priority 150"
        );
        assert_eq!(
            Action::Remove("web".to_owned()).command_line(&context),
            "rampire vm remove web"
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
    fn the_add_forms_changes_become_vm_adds_options() {
        let options = add_options(
            "web",
            &["priority=150".to_owned(), "max_ram=12GB".to_owned()],
        )
        .unwrap();
        assert_eq!(options.priority, 150);
        assert_eq!(options.max_ram.as_deref(), Some("12GB"));
        assert_eq!(options.guest_login, None);
        assert!(add_options("web", &["priority=lots".to_owned()]).is_err());
        // The TUI has no SSH login: the form can't give one.
        assert!(add_options("web", &["guest_login=root@web".to_owned()]).is_err());
    }

    #[test]
    fn a_findings_command_becomes_its_action() {
        let words =
            |text: &str| -> Vec<String> { text.split_whitespace().map(str::to_owned).collect() };
        assert_eq!(from_words(&words("apply")), Some(Action::Apply));
        assert_eq!(
            from_words(&words("vm restart web")),
            Some(Action::Restart("web".to_owned()))
        );
        assert_eq!(
            from_words(&words("recover web")),
            Some(Action::Recover("web".to_owned()))
        );
        assert_eq!(
            from_words(&words("set web alias=ua-virtiomem0")),
            Some(Action::Set {
                target: "web".to_owned(),
                changes: vec!["alias=ua-virtiomem0".to_owned()]
            })
        );
        assert_eq!(from_words(&words("doctor web")), None);
        assert_eq!(from_words(&words("set web")), None);
    }
}
