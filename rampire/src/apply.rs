//! Changing what the controller runs: `diff` shows the draft against the
//! applied copy, `apply` checks a copy of the draft and installs it, and
//! `rollback` returns to the copy before. Either change moves a running
//! controller onto the new copy, by reload or restart, and is recorded as
//! evidence.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rampire_host::config::HostSettings;
use rampire_ops::process;

use crate::controller::{controller_unit, systemctl};
use crate::{
    applied_root, confirm, daemon, draft, exit, libvirt, version, Cli, TempDir, SERVICE_ACCOUNT,
};

fn print_check(report: &rampire_ops::check::Report) {
    for vm in &report.vms {
        if let Some(problem) = &vm.problem {
            println!("problem: {}: {problem}", vm.vm_name);
            if problem.contains("drifted") {
                println!(
                    "  fix: if the change was intended, rampire vm attest {}",
                    vm.vm_name
                );
            }
        }
    }
}

/// Show the difference between two configuration directories, `None` being
/// no configuration at all. Returns whether there is any.
fn show_difference(from: Option<&Path>, to: &Path) -> Result<bool, String> {
    let empty;
    let from = match from {
        Some(path) => path,
        None => {
            empty = TempDir::new("empty")?;
            empty.path()
        }
    };
    // An attestation is a machine record of hashes: a person can't check it
    // line by line, so it is named rather than shown.
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
            // Name the two sides, not the temporary paths behind them.
            let text = String::from_utf8_lossy(&output.stdout)
                .replace(&from.display().to_string(), "applied")
                .replace(&to.display().to_string(), "draft");
            print!("{text}");
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
        println!("attestation of {vm}: {change}");
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
            None => changes.push((vm.clone(), "new, recording how the VM is set up now")),
            Some(old) if old != bytes => changes.push((vm.clone(), "changed")),
            Some(_) => {}
        }
    }
    for vm in before.keys().filter(|vm| !after.contains_key(*vm)) {
        changes.push((vm.clone(), "removed"));
    }
    changes.sort();
    Ok(changes)
}

pub(crate) fn diff(cli: &Cli) -> Result<ExitCode, String> {
    let temporary = TempDir::new("draft")?;
    let staged = temporary.path().join("draft");
    rampire_ops::applied::snapshot(&draft(cli), &staged)?;
    let current = rampire_ops::applied::current(applied_root())?;
    if !show_difference(current.as_deref(), &staged)? {
        println!("no changes: the draft is what the controller runs");
    }
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn apply(cli: &Cli, detailed: bool) -> Result<ExitCode, String> {
    let root = applied_root();
    let staged = rampire_ops::applied::staging(root)?;
    let result = stage_and_apply(cli, root, &staged);
    if staged.exists() {
        let _ = std::fs::remove_dir_all(&staged);
    }
    let changed = result?;
    Ok(ExitCode::from(if changed && detailed {
        exit::CHANGED
    } else {
        exit::SUCCESS
    }))
}

/// Returns whether the applied configuration changed.
fn stage_and_apply(cli: &Cli, root: &Path, staged: &Path) -> Result<bool, String> {
    // A VM with no attestation yet is attested here when its file names a
    // review profile and it runs (decision D11); a drifted one is not.
    for vm in rampire_ops::attest::attest_missing(&draft(cli), &libvirt(cli)?)? {
        println!("attested {vm}, which had no attestation");
    }
    rampire_ops::applied::snapshot(&draft(cli), staged)?;
    // Check the copy, so what is checked is exactly what is applied.
    let report = rampire_ops::check::run(staged)?;
    if !report.passed() {
        print_check(&report);
        return Err("the draft did not pass its checks; nothing changed".to_owned());
    }
    let current = rampire_ops::applied::current(root)?;
    if let Some(current) = &current {
        if rampire_ops::applied::same(current, staged)? {
            println!("nothing to apply: the draft is what the controller runs");
            return Ok(false);
        }
    }
    show_difference(current.as_deref(), staged)?;
    let unit = controller_unit();
    let refresh = Refresh::plan(&unit, current.as_deref(), staged);
    if !confirm(cli, &format!("Apply this{}?", refresh.question(&unit)))? {
        println!("nothing changed");
        return Ok(false);
    }
    secure(staged)?;
    let millis = process::unix_millis()?;
    let applied = rampire_ops::applied::install(root, staged, millis)?;
    println!("applied {}", applied.display());
    let refreshed = refresh.run(&unit)?;
    keep_evidence(Change {
        action: "apply",
        unix_millis: millis,
        draft: Some(&draft(cli)),
        from: current.as_deref(),
        to: &applied,
        report: &report,
        unit: &unit,
        refreshed,
    })?;
    Ok(true)
}

pub(crate) fn rollback(cli: &Cli) -> Result<ExitCode, String> {
    let root = applied_root();
    let current = rampire_ops::applied::current(root)?.ok_or("nothing has been applied yet")?;
    let copies = rampire_ops::applied::copies(root)?;
    let index = copies
        .iter()
        .position(|copy| copy == &current)
        .ok_or("the current copy is missing from the applied copies")?;
    let previous = index
        .checked_sub(1)
        .map(|index| copies[index].clone())
        .ok_or("there is no earlier applied copy")?;
    let report = rampire_ops::check::run(&previous)?;
    if !report.passed() {
        print_check(&report);
        return Err(format!(
            "{} no longer passes its checks; nothing changed",
            previous.display()
        ));
    }
    show_difference(Some(&current), &previous)?;
    let unit = controller_unit();
    let refresh = Refresh::plan(&unit, Some(&current), &previous);
    if !confirm(
        cli,
        &format!(
            "Go back to {}{}?",
            previous.display(),
            refresh.question(&unit)
        ),
    )? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    rampire_ops::applied::make_current(root, &previous)?;
    println!("rolled back to {}", previous.display());
    let refreshed = refresh.run(&unit)?;
    keep_evidence(Change {
        action: "rollback",
        unix_millis: process::unix_millis()?,
        draft: None,
        from: Some(&current),
        to: &previous,
        report: &report,
        unit: &unit,
        refreshed,
    })?;
    Ok(ExitCode::SUCCESS)
}

/// One change to the applied configuration, for its record.
struct Change<'a> {
    action: &'a str,
    unix_millis: u64,
    draft: Option<&'a Path>,
    from: Option<&'a Path>,
    to: &'a Path,
    report: &'a rampire_ops::check::Report,
    unit: &'a str,
    refreshed: Refreshed,
}

/// Record what the change did, then prune applied copies and records to
/// the `retention` of the copy now current.
fn keep_evidence(change: Change) -> Result<(), String> {
    use rampire_ops::evidence::{self, ApplyEvidence, ApplyEvidenceSchema};
    let evidence_root = Path::new(evidence::EVIDENCE_ROOT);
    let record = evidence::record(
        evidence_root,
        &ApplyEvidence {
            schema: ApplyEvidenceSchema,
            version: 1,
            action: change.action.to_owned(),
            unix_millis: change.unix_millis,
            build: version(),
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
    println!("recorded {}", record.display());
    let keep = rampire_core::settings::load_globals(change.to)?.retention as usize;
    rampire_ops::applied::prune(applied_root(), keep, running_copy().as_deref())?;
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
    /// Only what a reload can change differs (roadmap O4.5).
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

    fn run(self, unit: &str) -> Result<Refreshed, String> {
        match self {
            Self::NotRunning => {
                println!("{unit} isn't running; it uses this copy when it starts");
                Ok(Refreshed::NotRunning)
            }
            Self::Reload => {
                systemctl("reload", unit)?;
                println!("{unit} reloaded");
                Ok(Refreshed::Reloaded)
            }
            Self::Restart(why) => {
                systemctl("restart", unit)?;
                println!("{unit} restarted: {why}");
                Ok(Refreshed::Restarted)
            }
        }
    }
}

/// The copy the running controller started on, if it is running and says.
/// The record outlives a stopped controller, so it counts only while the
/// unit is active.
pub(crate) fn running_copy() -> Option<PathBuf> {
    if !controller_active(&controller_unit()) {
        return None;
    }
    std::fs::read_to_string(daemon::RUNNING_COPY)
        .ok()
        .map(|text| PathBuf::from(text.trim()))
}

fn controller_active(unit: &str) -> bool {
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
                (
                    "added".to_owned(),
                    "new, recording how the VM is set up now"
                ),
                ("edited".to_owned(), "changed"),
                ("gone".to_owned(), "removed"),
            ]
        );
        // No attestations folder on either side is no change.
        let empty = TempDir::new("test-empty").unwrap();
        assert!(attestation_changes(empty.path(), empty.path())
            .unwrap()
            .is_empty());
    }
}
