//! A VM's life under RAMpire: adding it ([`add`]), letting it go
//! ([`remove`]), upgrading its guest service and waiting for that to report
//! in ([`wait`]), and, here, restarting it onto its saved definition and
//! changing its settings with `set`, with the helpers for its definition
//! and settings files that every one of them uses.

mod add;
mod remove;
mod wait;

pub use add::{vm_add, AddOptions};
pub use remove::{forgotten_files, vm_remove};
pub use wait::vm_upgrade;
// What one flow uses of another's, here so each finds it through `super`.
use remove::still_managed_in;
use wait::{guest_reports_in, upgrade_if_behind};

use std::io::Write;
use std::path::{Path, PathBuf};

use super::apply::{apply, running_copy};
use super::Outcome;
use crate::context::{applied_root, Context, TempDir, DOMAIN_BACKUPS};
use crate::process;
use crate::ui::Ui;
use rampire_core::format_byte_size;
use rampire_host::attestation::AnswerSource;

/// Where a VM's settings are in the draft.
fn settings_path(directory: &Path, name: &str) -> Result<PathBuf, String> {
    Ok(directory
        .join(rampire_core::settings::VMS_DIRECTORY)
        .join(rampire_core::settings::vm_filename(name)?))
}

/// Save `fixed` as `name`'s definition, keeping `xml`, the one before, in
/// [`DOMAIN_BACKUPS`]. Returns the backup.
fn redefine(
    libvirt: &crate::attest::Libvirt,
    name: &str,
    xml: &str,
    fixed: &str,
) -> Result<PathBuf, String> {
    let backups = Path::new(DOMAIN_BACKUPS);
    std::fs::create_dir_all(backups).map_err(|error| error.to_string())?;
    let backup = backups.join(backup_name(name, process::unix_millis()?));
    std::fs::write(&backup, xml).map_err(|error| format!("keep {}: {error}", backup.display()))?;
    define(libvirt, name, fixed)?;
    Ok(backup)
}

/// The file in [`DOMAIN_BACKUPS`] that keeps `name`'s definition as it was
/// before `vm add` changed it at `unix_millis`.
fn backup_name(name: &str, unix_millis: u64) -> String {
    format!(
        "{}-{unix_millis}.xml",
        rampire_core::settings::vm_file_stem(name)
    )
}

/// Whether `file` is one of `name`'s backups: not another VM's whose name
/// starts with it (`web-2-...` for `web`).
fn is_backup_of(file: &str, name: &str) -> bool {
    file.strip_prefix(rampire_core::settings::vm_file_stem(name).as_str())
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".xml"))
        .is_some_and(|millis| !millis.is_empty() && millis.bytes().all(|b| b.is_ascii_digit()))
}

/// Save `xml` as `name`'s definition.
fn define(libvirt: &crate::attest::Libvirt, name: &str, xml: &str) -> Result<(), String> {
    let temporary = TempDir::new(name)?;
    let path = temporary.path().join("definition.xml");
    std::fs::write(&path, xml).map_err(|error| error.to_string())?;
    libvirt.virsh(&["define", &path_text(&path)?])?;
    Ok(())
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

/// `rampire vm restart`: shut the VM down and start it again, so its saved
/// definition takes effect, then apply, which records its new setup.
pub fn vm_restart(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<Outcome, String> {
    crate::attest::vm_settings(&context.draft(), vm)?;
    if !ui.confirm(&format!(
        "Shut {vm} down and start it again, so its saved definition takes effect?"
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let libvirt = context.libvirt()?;
    crate::attest::cold_restart(vm, &libvirt, std::time::Duration::from_secs(300))?;
    ui.say(&format!("{vm} restarted"));
    crate::audit::record(context, "vm_restart", Some(vm), &[]);
    Ok(match apply(context, ui)? {
        Outcome::Declined(_) => Outcome::Partly(format!(
            "{vm} restarted, but its new setup isn't applied: rampire apply records it"
        )),
        _ => Outcome::Done,
    })
}

/// `rampire set`: change settings in the draft, then apply unless
/// `no_apply`. A VM whose guest runs another build than this host's has
/// its guest upgraded as well. [`Outcome::Partly`] when the change is
/// saved but not applied, other than with `no_apply`, which asks for
/// that.
pub fn set(
    context: &Context,
    ui: &mut dyn Ui,
    target: &str,
    changes: &[crate::set::Assignment],
    no_apply: bool,
) -> Result<Outcome, String> {
    let path = crate::set::set(&context.draft(), target, changes)?;
    ui.say(&format!("changed {}", path.display()));
    let described: Vec<String> = changes
        .iter()
        .map(|change| {
            let value = change.value.to_string();
            format!("{}={}", change.key, value.trim().trim_matches('"'))
        })
        .collect();
    crate::audit::record(
        context,
        "set",
        (target != crate::set::GLOBAL).then_some(target),
        &[("changes", &described.join(","))],
    );
    let saved = |error: String| {
        format!(
            "{error}\nThe change is saved in {} but not applied: fix what is said above and run rampire apply, or set the old value back",
            path.display()
        )
    };
    if target != crate::set::GLOBAL {
        if changes
            .iter()
            .any(|change| matches!(change.key.as_str(), "min_ram" | "max_ram"))
        {
            fit_to_limits(context, ui, target).map_err(saved)?;
        }
        upgrade_if_behind(context, ui, target).map_err(saved)?;
    }
    let not_applied = format!(
        "saved in {}, not applied: rampire apply applies it",
        path.display()
    );
    if no_apply {
        ui.say("not applied: run rampire apply when ready");
        return Ok(Outcome::Done);
    }
    Ok(match apply(context, ui).map_err(saved)? {
        Outcome::Declined(_) => Outcome::Partly(not_applied),
        outcome => outcome,
    })
}

/// After `min_ram` or `max_ram` changed: when the VM's definition can't
/// hold them, because it boots with other than its minimum or its device
/// can't reach its maximum, fit it to them, as `vm add` does. That takes
/// effect when the VM next starts from off, so ask whether to restart it
/// now. Until then its guest service keeps within both as nearly as it
/// can. A device bigger than the maximum needs nothing: the controller
/// never grows the VM past it.
fn fit_to_limits(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<(), String> {
    use std::time::Duration;
    let settings = crate::attest::vm_settings(&context.draft(), vm)?;
    let libvirt = context.libvirt()?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", vm])?;
    let inspection = crate::vm::inspect(&xml)?;
    let (Some(holds), Some(device)) = (inspection.holds(), inspection.device()) else {
        return Ok(());
    };
    let fit = crate::vm::fitted(settings.min_ram, settings.max_ram, device.block_bytes)?;
    if holds.boot == fit.boot && holds.device >= fit.device {
        return Ok(());
    }
    let fitted = crate::vm::fit_memory(&xml, settings.min_ram, settings.max_ram)?;
    let backup = redefine(&libvirt, vm, &xml, &fitted)?;
    crate::audit::record(
        context,
        "fix_xml",
        Some(vm),
        &[("backup", &backup.display().to_string())],
    );
    ui.say(&format!(
        "{vm} now boots with {}, its minimum, and can grow to {}; the old definition is in {}",
        format_byte_size(fit.boot),
        format_byte_size(fit.boot + fit.device),
        backup.display()
    ));
    if libvirt.state(vm)? != "running" {
        ui.say(&format!("it takes effect when {vm} starts"));
        return Ok(());
    }
    if ui.confirm(&format!(
        "That takes a restart. Shut {vm} down and start it again now? (No: at its next start)"
    ))? {
        crate::attest::cold_restart(vm, &libvirt, Duration::from_secs(300))?;
        ui.say(&format!("{vm} restarted"));
    } else {
        ui.say(&format!(
            "until {vm} next starts from off it keeps within its new sizes as nearly as it can, from {} to {}; rampire vm restart {vm} restarts it",
            format_byte_size(holds.boot),
            format_byte_size(holds.boot + holds.device)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Transcript;

    pub(super) fn context(draft: &Path) -> Context {
        Context {
            config: Some(draft.to_path_buf()),
            connect: Some("test:///default".to_owned()),
            build: String::new(),
            via: "command",
        }
    }

    #[test]
    fn a_vms_backups_are_its_own() {
        let backup = backup_name("web", 1_700_000_000_000);
        assert!(is_backup_of(&backup, "web"));
        assert!(!is_backup_of(
            &backup_name("web-2", 1_700_000_000_000),
            "web"
        ));
        assert!(!is_backup_of("web-notes.xml", "web"));
        assert!(!is_backup_of("web-.xml", "web"));
        assert!(!is_backup_of(&backup, "we"));
        // A capital letter is written as in the VM's other files.
        let capital = backup_name("Web", 1_700_000_000_000);
        assert_eq!(capital, "^web-1700000000000.xml");
        assert!(is_backup_of(&capital, "Web"));
        assert!(!is_backup_of(&capital, "web"));
        assert!(!is_backup_of(&backup, "Web"));
    }

    #[test]
    fn declining_changes_nothing_and_says_so() {
        // Each flow asks before it changes anything: a No ends it, and the
        // outcome says so, not that it was done.
        let draft = TempDir::new("test-decline").unwrap();
        let vms = draft.path().join(rampire_core::settings::VMS_DIRECTORY);
        std::fs::create_dir_all(&vms).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/config/linux-guest.toml");
        std::fs::copy(fixture, vms.join("web.toml")).unwrap();
        let mut ui = Transcript::answering(&["n", "n"]);
        let outcome = vm_remove(&context(draft.path()), &mut ui, "web").unwrap();
        assert_eq!(outcome, Outcome::declined());
        assert!(vms.join("web.toml").exists(), "still managed");
        assert_eq!(
            vm_restart(&context(draft.path()), &mut ui, "web").unwrap(),
            Outcome::declined()
        );
    }

    #[test]
    fn only_a_managed_vm_is_upgraded() {
        let draft = TempDir::new("test-upgrade").unwrap();
        let mut ui = Transcript::default();
        assert!(vm_upgrade(&context(draft.path()), &mut ui, "web", None).is_err());
        assert!(ui.said.is_empty());
    }
}
