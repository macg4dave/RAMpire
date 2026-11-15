//! What a person can do from the TUI. Each action is one of the commands'
//! own flows, run with the TUI's dialogs answering it, and names the command
//! line that does the same, so a person learns the commands and can script
//! what they did.

use super::jobs::{self, ChannelUi, Msg, ReaderControl, TaskId};
use crate::context::Context;
use crate::flows::{apply, controller, lifecycle};
use crate::ui::Ui;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Sender;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `vm add VM`, with the settings the form changed as `KEY=VALUE`:
    /// `priority`, `min_ram`, `max_ram`, `free_ram` and `guest_login`.
    Add {
        vm: String,
        changes: Vec<String>,
    },
    /// `vm remove VM`.
    Remove(String),
    /// `vm restart VM`, so its saved definition takes effect.
    Restart(String),
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

/// The `vm add` option for a setting the add form names.
fn add_flag(key: &str) -> Option<&'static str> {
    Some(match key {
        "priority" => "--priority",
        "min_ram" => "--min-ram",
        "max_ram" => "--max-ram",
        "free_ram" => "--free-ram",
        "guest_login" => "--guest-login",
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
            Self::Add { vm, .. } | Self::Remove(vm) | Self::Restart(vm) | Self::Recover(vm) => {
                Some(vm)
            }
            Self::Set { target, .. } if target != crate::set::GLOBAL => Some(target),
            _ => None,
        }
    }

    /// Run it on its own thread as task `id`. The reader reloads the
    /// configuration when it ends, whatever it changed.
    pub fn spawn(&self, context: &Context, control: &ReaderControl, tx: Sender<Msg>, id: TaskId) {
        let action = self.clone();
        let context = context.clone();
        let reload = control.reload.clone();
        jobs::spawn_flow(tx, id, move |ui| {
            let result = action.run(&context, ui);
            reload.store(true, Ordering::Release);
            result
        });
    }

    fn run(&self, context: &Context, ui: &mut ChannelUi) -> Result<String, String> {
        match self {
            Self::Add { vm, changes } => {
                lifecycle::vm_add(context, ui, &add_options(vm, changes)?)?;
                Ok(format!("{vm} is added"))
            }
            Self::Remove(vm) => {
                lifecycle::vm_remove(context, ui, vm)?;
                Ok(format!("{vm} is removed"))
            }
            Self::Restart(vm) => {
                lifecycle::vm_restart(context, ui, vm)?;
                Ok(format!("{vm} restarted"))
            }
            Self::Set { target, changes } => {
                let changes = changes
                    .iter()
                    .map(|change| crate::set::parse_assignment(change))
                    .collect::<Result<Vec<_>, _>>()?;
                lifecycle::set(context, ui, target, &changes, false)?;
                Ok("the settings are saved and applied".to_owned())
            }
            Self::Apply => {
                apply::apply(context, ui)?;
                Ok("the configuration is applied".to_owned())
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
                Ok(format!("{vm} resumes resizing"))
            }
            Self::Init => {
                controller::init(context, ui)?;
                Ok("the host is set up".to_owned())
            }
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
            "free_ram" => options.free_ram = Some(value),
            "guest_login" if !value.is_empty() => options.guest_login = Some(value),
            "guest_login" => {}
            _ => return Err(format!("vm add has no setting {key}")),
        }
    }
    Ok(options)
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
        ["vm", "add", vm] => Some(Action::add(vm)),
        ["vm", "restart", vm] => Some(Action::Restart((*vm).to_owned())),
        ["vm", "remove", vm] => Some(Action::Remove((*vm).to_owned())),
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
            Action::add("webserver").command_line(&context),
            "rampire vm add webserver"
        );
        assert_eq!(
            Action::Add {
                vm: "web".to_owned(),
                changes: vec![
                    "max_ram=12GB".to_owned(),
                    "guest_login=admin@web".to_owned()
                ]
            }
            .command_line(&context),
            "rampire vm add web --max-ram 12GB --guest-login admin@web"
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
            &[
                "priority=150".to_owned(),
                "max_ram=12GB".to_owned(),
                "guest_login=".to_owned(),
            ],
        )
        .unwrap();
        assert_eq!(options.priority, 150);
        assert_eq!(options.max_ram.as_deref(), Some("12GB"));
        assert_eq!(options.guest_login, None);
        assert!(add_options("web", &["priority=lots".to_owned()]).is_err());
    }

    #[test]
    fn a_fix_that_is_a_whole_command_can_be_run() {
        assert_eq!(from_fix("rampire apply"), Some(Action::Apply));
        assert_eq!(
            from_fix("rampire vm restart web"),
            Some(Action::Restart("web".to_owned()))
        );
        assert_eq!(
            from_fix("rampire init on a new host, or rampire apply"),
            Some(Action::Init)
        );
        assert_eq!(
            from_fix("once the guest is checked, rampire recover web \"WHY IT IS SAFE TO RESUME\""),
            Some(Action::Recover("web".to_owned()))
        );
        assert_eq!(from_fix("rampire vm add web"), Some(Action::add("web")));
        // A placeholder to fill in, or no rampire command at all, is not run.
        assert_eq!(
            from_fix("raise max_ram: rampire set web max_ram=SIZE"),
            None
        );
        assert_eq!(from_fix("boot the guest with psi=1"), None);
        assert_eq!(from_fix("rampire doctor web"), None);
        assert_eq!(
            from_fix("rampire vm remove web, then rampire vm add web"),
            None
        );
    }
}
