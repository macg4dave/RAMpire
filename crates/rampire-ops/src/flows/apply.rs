//! Changing what the controller runs: `diff` shows the draft against the
//! applied copy, `apply` checks a copy of the draft and installs it, and
//! `rollback` returns to the copy before. Either change moves a running
//! controller onto the new copy, by reload or restart, and is recorded as
//! evidence.

use std::path::{Path, PathBuf};

use rampire_host::config::HostSettings;

use super::controller::{controller_unit, systemctl};
use super::Outcome;
use crate::context::{applied_root, Context, TempDir, RUNNING_COPY, SERVICE_ACCOUNT};
use crate::process;
use crate::ui::Ui;

/// Show the difference between two configuration directories, `None` being
/// no configuration at all. Returns whether there is any.
fn show_difference(ui: &mut dyn Ui, from: Option<&Path>, to: &Path) -> Result<bool, String> {
    let empty;
    let from = match from {
        Some(path) => path,
        None => {
            empty = TempDir::new("empty")?;
            empty.path()
        }
    };
    // A VM's recorded setup is a machine record of hashes: a person can't
    // check it line by line, so it is named rather than shown.
    let output = std::process::Command::new("diff")
        .arg("-ruN")
        .arg(format!("--exclude=*{ATTESTATION_SUFFIX}"))
        .arg(from)
        .arg(to)
        .output()
        .map_err(|error| format!("run diff: {error}"))?;
    let shown = match output.status.code() {
        Some(0) => false,
        Some(1) => {
            // Name the two sides, not the temporary paths behind them, and
            // leave out diff's own command line before each file.
            let text: String = String::from_utf8_lossy(&output.stdout)
                .replace(&from.display().to_string(), "applied")
                .replace(&to.display().to_string(), "draft")
                .lines()
                .filter(|line| !line.starts_with("diff -ruN"))
                .map(|line| format!("{line}\n"))
                .collect();
            ui.show(&text);
            true
        }
        _ => {
            return Err(format!(
                "diff failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    };
    let attestations = attestation_changes(from, to)?;
    for (vm, change) in &attestations {
        ui.say(&format!("{vm}: {change}"));
    }
    Ok(shown || !attestations.is_empty())
}

const ATTESTATION_SUFFIX: &str = ".attestation.json";

/// Each VM whose attestation differs between two configuration
/// directories, with what changed: new, changed or removed.
fn attestation_changes(from: &Path, to: &Path) -> Result<Vec<(String, &'static str)>, String> {
    let read = |root: &Path| -> Result<std::collections::BTreeMap<String, Vec<u8>>, String> {
        let directory = root.join("attestations");
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Default::default())
            }
            Err(error) => return Err(format!("read {}: {error}", directory.display())),
        };
        let mut found = std::collections::BTreeMap::new();
        for entry in entries {
            let path = entry.map_err(|error| error.to_string())?.path();
            let Some(vm) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(ATTESTATION_SUFFIX))
            else {
                continue;
            };
            let bytes = std::fs::read(&path)
                .map_err(|error| format!("read {}: {error}", path.display()))?;
            found.insert(vm.to_owned(), bytes);
        }
        Ok(found)
    };
    let (before, after) = (read(from)?, read(to)?);
    let mut changes = Vec::new();
    for (vm, bytes) in &after {
        match before.get(vm) {
            None => changes.push((vm.clone(), "records how it is set up now")),
            Some(old) if old != bytes => changes.push((vm.clone(), "records how it is set up now")),
            Some(_) => {}
        }
    }
    for vm in before.keys().filter(|vm| !after.contains_key(*vm)) {
        changes.push((vm.clone(), "drops its recorded setup"));
    }
    changes.sort();
    Ok(changes)
}

/// `rampire diff`: the draft against the applied copy.
pub fn diff(context: &Context, ui: &mut dyn Ui) -> Result<(), String> {
    let temporary = TempDir::new("draft")?;
    let staged = temporary.path().join("draft");
    crate::applied::snapshot(&context.draft(), &staged)?;
    let current = crate::applied::current(applied_root())?;
    if !show_difference(ui, current.as_deref(), &staged)? {
        ui.say("no changes: the draft is what the controller runs");
    }
    Ok(())
}

/// `rampire apply`: [`Outcome::Done`] when the applied configuration
/// changed.
pub fn apply(context: &Context, ui: &mut dyn Ui) -> Result<Outcome, String> {
    apply_asking(context, ui, true)
}

/// `apply` for a change the person already agreed to, such as `vm add`'s:
/// the difference isn't shown and nothing is asked again.
pub fn apply_agreed(context: &Context, ui: &mut dyn Ui) -> Result<Outcome, String> {
    apply_asking(context, ui, false)
}

fn apply_asking(context: &Context, ui: &mut dyn Ui, ask: bool) -> Result<Outcome, String> {
    let root = applied_root();
    let staged = crate::applied::staging(root)?;
    let result = stage_and_apply(context, ui, root, &staged, ask);
    if staged.exists() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    result
}

fn stage_and_apply(
    context: &Context,
    ui: &mut dyn Ui,
    root: &Path,
    staged: &Path,
    ask: bool,
) -> Result<Outcome, String> {
    let draft = context.draft();
    // Each running VM's recorded setup is brought up to date first, so
    // what is checked is the VMs as they are now.
    for vm in crate::attest::keep_current(&draft, &context.libvirt()?, ui)? {
        crate::audit::record(context, "vm_recorded", Some(&vm), &[]);
    }
    crate::applied::snapshot(&draft, staged)?;
    // Check the copy, so what is checked is exactly what is applied.
    let report = crate::check::run(staged)?;
    if !report.passed {
        ui.say(crate::check::text(&report, true).trim_end());
        return Err("the draft did not pass its checks; nothing changed".to_owned());
    }
    let current = crate::applied::current(root)?;
    if let Some(current) = &current {
        if crate::applied::same(current, staged)? {
            let same = "nothing to apply: the draft is what the controller runs";
            ui.say(same);
            return Ok(Outcome::AlreadySo(same.to_owned()));
        }
    }
    let unit = controller_unit();
    let refresh = Refresh::plan(&unit, current.as_deref(), staged);
    if ask {
        show_difference(ui, current.as_deref(), staged)?;
        if !ui.confirm(&format!("Apply this{}?", refresh.question(&unit)))? {
            ui.say("nothing changed");
            return Ok(Outcome::Declined(
                "nothing was applied; the draft keeps its changes for rampire apply".to_owned(),
            ));
        }
    }
    secure(staged)?;
    let millis = process::unix_millis()?;
    let applied = crate::applied::install(root, staged, millis)?;
    ui.say(&format!("applied {}", applied.display()));
    let refreshed = refresh.run(ui, &unit)?;
    crate::audit::record(
        context,
        "apply",
        None,
        &[("to", &applied.display().to_string())],
    );
    keep_evidence(
        context,
        ui,
        Change {
            action: "apply",
            unix_millis: millis,
            draft: Some(&draft),
            from: current.as_deref(),
            to: &applied,
            report: &report,
            unit: &unit,
            refreshed,
        },
    )?;
    Ok(Outcome::Done)
}

/// `rampire rollback`: back to the applied copy before the current one.
pub fn rollback(context: &Context, ui: &mut dyn Ui) -> Result<(), String> {
    let root = applied_root();
    let current = crate::applied::current(root)?.ok_or("nothing has been applied yet")?;
    let copies = crate::applied::copies(root)?;
    let index = copies
        .iter()
        .position(|copy| copy == &current)
        .ok_or("the current copy is missing from the applied copies")?;
    let previous = index
        .checked_sub(1)
        .map(|index| copies[index].clone())
        .ok_or("there is no earlier applied copy")?;
    let report = crate::check::run(&previous)?;
    if !report.passed {
        ui.say(crate::check::text(&report, true).trim_end());
        return Err(format!(
            "{} no longer passes its checks; nothing changed",
            previous.display()
        ));
    }
    show_difference(ui, Some(&current), &previous)?;
    let unit = controller_unit();
    let refresh = Refresh::plan(&unit, Some(&current), &previous);
    if !ui.confirm(&format!(
        "Go back to {}{}?",
        previous.display(),
        refresh.question(&unit)
    ))? {
        ui.say("nothing changed");
        return Ok(());
    }
    crate::applied::make_current(root, &previous)?;
    ui.say(&format!("rolled back to {}", previous.display()));
    let refreshed = refresh.run(ui, &unit)?;
    crate::audit::record(
        context,
        "rollback",
        None,
        &[("to", &previous.display().to_string())],
    );
    keep_evidence(
        context,
        ui,
        Change {
            action: "rollback",
            unix_millis: process::unix_millis()?,
            draft: None,
            from: Some(&current),
            to: &previous,
            report: &report,
            unit: &unit,
            refreshed,
        },
    )?;
    Ok(())
}

/// One change to the applied configuration, for its record.
struct Change<'a> {
    action: &'a str,
    unix_millis: u64,
    draft: Option<&'a Path>,
    from: Option<&'a Path>,
    to: &'a Path,
    report: &'a crate::reports::CheckReport,
    unit: &'a str,
    refreshed: Refreshed,
}

/// Record what the change did, then prune applied copies and records to
/// the `retention` of the copy now current.
fn keep_evidence(context: &Context, ui: &mut dyn Ui, change: Change) -> Result<(), String> {
    use crate::evidence::{self, ApplyEvidence, ApplyEvidenceSchema};
    let evidence_root = Path::new(evidence::EVIDENCE_ROOT);
    let record = evidence::record(
        evidence_root,
        &ApplyEvidence {
            schema: ApplyEvidenceSchema,
            version: 1,
            action: change.action.to_owned(),
            unix_millis: change.unix_millis,
            build: context.build.clone(),
            draft: change.draft.map(|path| path.display().to_string()),
            from: change.from.map(|path| path.display().to_string()),
            to: change.to.display().to_string(),
            files: evidence::digests(change.to)?,
            checked_vms: change
                .report
                .vms
                .iter()
                .map(|vm| vm.vm_name.clone())
                .collect(),
            unit: change.unit.to_owned(),
            restarted: change.refreshed == Refreshed::Restarted,
            reloaded: change.refreshed == Refreshed::Reloaded,
        },
    )?;
    ui.say(&format!("recorded {}", record.display()));
    let keep = rampire_core::settings::load_globals(change.to)?.retention as usize;
    crate::applied::prune(applied_root(), keep, running_copy().as_deref())?;
    evidence::prune(evidence_root, keep)?;
    Ok(())
}

/// The controller reads the copy as the service account; nobody else does.
fn secure(copy: &Path) -> Result<(), String> {
    let cwd = Path::new("/");
    let timeout = std::time::Duration::from_secs(30);
    for (program, args) in [
        ("chgrp", ["-R", SERVICE_ACCOUNT]),
        ("chmod", ["-R", "u=rwX,g=rX,o="]),
    ] {
        let mut arguments: Vec<_> = args.iter().map(process::os).collect();
        arguments.push(copy.as_os_str().to_owned());
        process::bounded_text(program, &arguments, cwd, timeout)?;
    }
    Ok(())
}

/// What became of a running controller after the applied copy changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refreshed {
    Restarted,
    Reloaded,
    NotRunning,
}

/// How a change reaches the controller, worked out before the person is
/// asked so the question says what will happen.
enum Refresh {
    /// Only what a reload can change differs.
    Reload,
    /// Anything else, with the reason.
    Restart(String),
    /// The unit isn't running: it starts on the new copy whenever it starts.
    NotRunning,
}

impl Refresh {
    /// Plan moving a running controller from the copy `from` onto `to`.
    fn plan(unit: &str, from: Option<&Path>, to: &Path) -> Self {
        if !controller_active(unit) {
            return Self::NotRunning;
        }
        let reload = from
            .ok_or_else(|| "nothing was applied before".to_owned())
            .and_then(|from| {
                let load =
                    |path: &Path| HostSettings::load(path).map_err(|error| error.to_string());
                load(from)?.reloadable_to(&load(to)?)
            });
        match reload {
            Ok(()) => Self::Reload,
            Err(why) => Self::Restart(why),
        }
    }

    /// What the question adds after its action, such as "and reload rampire.service".
    fn question(&self, unit: &str) -> String {
        match self {
            Self::Reload => format!(" and reload {unit}"),
            Self::Restart(_) => format!(" and restart {unit}"),
            Self::NotRunning => String::new(),
        }
    }

    fn run(self, ui: &mut dyn Ui, unit: &str) -> Result<Refreshed, String> {
        match self {
            Self::NotRunning => {
                ui.say(&format!(
                    "{unit} isn't running; it uses this copy when it starts"
                ));
                Ok(Refreshed::NotRunning)
            }
            Self::Reload => {
                systemctl("reload", unit)?;
                ui.say(&format!("{unit} reloaded"));
                Ok(Refreshed::Reloaded)
            }
            Self::Restart(why) => {
                systemctl("restart", unit)?;
                ui.say(&format!("{unit} restarted: {why}"));
                Ok(Refreshed::Restarted)
            }
        }
    }
}

/// The copy the running controller started on, if it is running and says.
/// The record outlives a stopped controller, so it counts only while the
/// unit is active.
pub fn running_copy() -> Option<PathBuf> {
    if !controller_active(&controller_unit()) {
        return None;
    }
    std::fs::read_to_string(RUNNING_COPY)
        .ok()
        .map(|text| PathBuf::from(text.trim()))
}

pub fn controller_active(unit: &str) -> bool {
    process::bounded_text(
        "systemctl",
        &[process::os("is-active"), process::os(unit)],
        Path::new("/"),
        std::time::Duration::from_secs(10),
    )
    .is_ok_and(|state| state.trim() == "active")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{Said, Transcript};

    #[test]
    fn attestations_are_named_as_new_changed_or_removed_not_shown() {
        let from = TempDir::new("test-from").unwrap();
        let to = TempDir::new("test-to").unwrap();
        let write = |root: &Path, vm: &str, text: &str| {
            let directory = root.join("attestations");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(format!("{vm}{ATTESTATION_SUFFIX}")), text).unwrap();
        };
        write(from.path(), "same", "{}");
        write(to.path(), "same", "{}");
        write(from.path(), "edited", "{\"a\":1}");
        write(to.path(), "edited", "{\"a\":2}");
        write(from.path(), "gone", "{}");
        write(to.path(), "added", "{}");
        assert_eq!(
            attestation_changes(from.path(), to.path()).unwrap(),
            [
                ("added".to_owned(), "records how it is set up now"),
                ("edited".to_owned(), "records how it is set up now"),
                ("gone".to_owned(), "drops its recorded setup"),
            ]
        );
        // No attestations folder on either side is no change.
        let empty = TempDir::new("test-empty").unwrap();
        assert!(attestation_changes(empty.path(), empty.path())
            .unwrap()
            .is_empty());
    }

    #[test]
    fn a_difference_is_shown_as_one_change_with_its_sides_named() {
        let from = TempDir::new("test-show-from").unwrap();
        let to = TempDir::new("test-show-to").unwrap();
        std::fs::write(from.path().join("global.toml"), "a = 1\n").unwrap();
        std::fs::write(to.path().join("global.toml"), "a = 2\n").unwrap();
        let mut ui = Transcript::default();
        assert!(show_difference(&mut ui, Some(from.path()), to.path()).unwrap());
        let [Said::Shown(text)] = ui.said.as_slice() else {
            panic!("{:?}", ui.said);
        };
        assert!(text.contains("applied/global.toml"), "{text}");
        assert!(text.contains("draft/global.toml"), "{text}");
        assert!(text.contains("-a = 1\n+a = 2\n"), "{text}");

        let mut ui = Transcript::default();
        assert!(!show_difference(&mut ui, Some(to.path()), to.path()).unwrap());
        assert!(ui.said.is_empty());
    }
}
