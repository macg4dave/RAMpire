//! Making attestations (docs/administration.md, adding a VM). An attestation binds a VM's
//! review profile to its live evidence; it lands in the draft's
//! `attestations/` folder, so the next applied copy carries it.
//!
//! People never see or make one: `apply` keeps every running VM's record
//! current ([`keep_current`]).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rampire_core::settings::{self, VmSettings, VMS_DIRECTORY};
use rampire_host::attestation::{
    read_attestation, read_review, CompatibilityAttestation, VirshCompatibilityEvidenceSource,
};
use rampire_host::config::{attestation_path, ATTESTATION_DIRECTORY};
use rampire_host::virsh::{Virsh, VirshCommand};

use crate::applied::REVIEW_DIRECTORY;
use crate::ui::Ui;

/// What the commands need to reach libvirt.
#[derive(Debug, Clone)]
pub struct Libvirt {
    pub connection: String,
    pub timeout: Duration,
}

impl Libvirt {
    /// The host's `virsh` runner on this connection.
    pub fn runner(&self) -> Virsh {
        Virsh::with_connection("virsh", self.timeout, &self.connection)
    }

    /// Run `virsh` on this connection.
    pub fn virsh(&self, args: &[&str]) -> Result<String, String> {
        let arguments: Vec<String> = args.iter().map(|&argument| argument.to_owned()).collect();
        self.runner()
            .run(&arguments)
            .map_err(|error| error.to_string())
    }

    /// The VM's state as `virsh domstate` names it: "running", "shut off".
    pub fn state(&self, vm: &str) -> Result<String, String> {
        Ok(self.virsh(&["domstate", vm])?.trim().to_owned())
    }
}

/// The VM's settings file in the draft.
pub fn vm_settings(directory: &Path, vm: &str) -> Result<VmSettings, String> {
    settings::load_vm(
        &directory
            .join(VMS_DIRECTORY)
            .join(settings::vm_filename(vm)?),
    )
}

/// Attest the running VM `vm` from its review profile. Returns where the
/// attestation was written.
pub fn attest(directory: &Path, vm: &str, libvirt: &Libvirt) -> Result<PathBuf, String> {
    let settings = vm_settings(directory, vm)?;
    let profile = settings.review.as_deref().ok_or_else(|| {
        format!("{vm} names no review profile: add review = \"PROFILE\" to its file")
    })?;
    let review = read_review(
        &directory
            .join(REVIEW_DIRECTORY)
            .join(format!("{profile}.toml")),
    )?;
    if review.guest_os != settings.guest_os {
        return Err(format!(
            "review profile {profile} is for a {:?} guest, but {vm} is {:?}",
            review.guest_os, settings.guest_os
        ));
    }
    let state = libvirt.state(vm)?;
    if state != "running" {
        return Err(format!("{vm} is {state}: a VM is attested while it runs"));
    }
    let live =
        VirshCompatibilityEvidenceSource::new(libvirt.runner(), vm, &settings.alias).collect()?;
    let document = CompatibilityAttestation::new(live, review)?;
    let path = attestation_path(directory, vm);
    write_atomically(
        &path,
        format!("{}\n", document.to_pretty_json()?).as_bytes(),
    )?;
    Ok(path)
}

/// Keep the record of how each VM is set up current, so no person has to:
/// a running VM that names a review profile is recorded when it has no
/// record yet, or when it changed since (libvirt upgraded, its definition
/// edited) and what its review read from the definition still holds.
/// When a change makes it unsafe to resize, that is said and its old
/// record kept, so `check` and the controller refuse it. Returns the VMs
/// recorded.
pub fn keep_current(
    directory: &Path,
    libvirt: &Libvirt,
    ui: &mut dyn Ui,
) -> Result<Vec<String>, String> {
    let (_, vms) = settings::load_configuration(directory)?;
    let mut recorded = Vec::new();
    for vm in vms {
        let name = vm.vm_name.as_str();
        if vm.review.is_none() || libvirt.state(name).ok().as_deref() != Some("running") {
            continue;
        }
        let path = attestation_path(directory, name);
        if path.exists() {
            let live = VirshCompatibilityEvidenceSource::new(libvirt.runner(), name, &vm.alias)
                .collect()?;
            let unchanged = read_attestation(&path)
                .is_ok_and(|attestation| attestation.validate_live(&live, vm.guest_os).is_ok());
            if unchanged || !still_safe(libvirt, ui, name)? {
                continue;
            }
        }
        attest(directory, name, libvirt)?;
        recorded.push(vm.vm_name);
    }
    Ok(recorded)
}

/// Whether a VM that changed can still be resized: its definition is
/// read again for what the review answered from it. What it can't show
/// is asked; the review's question about the guest's programs is not,
/// since a change to the setup doesn't change them.
fn still_safe(libvirt: &Libvirt, ui: &mut dyn Ui, vm: &str) -> Result<bool, String> {
    let inspection = crate::vm::inspect(&libvirt.virsh(&["dumpxml", vm])?)?;
    if let Some((problem, fix)) = inspection.problems().into_iter().next() {
        ui.say(&format!("{vm} changed and can't be resized: {problem}"));
        ui.say(&format!("  fix: {fix}"));
        return Ok(false);
    }
    for detected in crate::vm::review_answers(&inspection, crate::vm::host_has_rdma()) {
        if detected.question == "workload_reviewed" {
            continue;
        }
        let safe = match detected.answer {
            Some((value, _)) => value,
            // Without a terminal to ask on, the VM waits for a person.
            None => ui
                .review(&format!(
                    "{vm} changed. {}",
                    crate::vm::question_text(detected.question)
                ))
                .unwrap_or(false),
        };
        if !safe {
            ui.say(&format!(
                "{vm} changed and can't be resized: {}",
                crate::vm::unsafe_text(detected.question)
            ));
            return Ok(false);
        }
    }
    Ok(true)
}

/// Shut the VM down, wait for it to stop, and start it again, so its saved
/// definition takes effect.
pub fn cold_restart(vm: &str, libvirt: &Libvirt, shutdown_timeout: Duration) -> Result<(), String> {
    if libvirt.state(vm)? != "shut off" {
        libvirt.virsh(&["shutdown", vm])?;
        let started = Instant::now();
        while libvirt.state(vm)? != "shut off" {
            if started.elapsed() > shutdown_timeout {
                return Err(format!(
                    "{vm} did not shut down within {} s; nothing more was done",
                    shutdown_timeout.as_secs()
                ));
            }
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    libvirt.virsh(&["start", vm])?;
    Ok(())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    rampire_host::files::replace_file(path, bytes, None, false)
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// The attestations folder, for messages.
pub fn folder(directory: &Path) -> PathBuf {
    directory.join(ATTESTATION_DIRECTORY)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(review: Option<&str>) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "rampire-attest-{}-{}",
            review.unwrap_or("none"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vms.d")).unwrap();
        let review = review
            .map(|name| format!("review = \"{name}\"\n"))
            .unwrap_or_default();
        std::fs::write(
            root.join("vms.d/web.toml"),
            format!("alias = \"m\"\npriority = 1\nguest_os = \"linux\"\nmin_ram = \"1GB\"\nmax_ram = \"4GB\"\nfree_ram = \"512MB\"\n{review}"),
        )
        .unwrap();
        root
    }

    fn unreachable() -> Libvirt {
        Libvirt {
            connection: "test:///no-such-connection".to_owned(),
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn a_vm_without_a_review_profile_cannot_be_attested() {
        let root = configuration(None);
        let error = attest(&root, "web", &unreachable()).unwrap_err();
        let recorded = keep_current(&root, &unreachable(), &mut crate::ui::Transcript::default());
        std::fs::remove_dir_all(&root).unwrap();
        assert!(error.contains("review profile"), "{error}");
        assert_eq!(recorded, Ok(vec![]), "nothing to record without a profile");
    }

    #[test]
    fn a_missing_profile_is_reported_before_libvirt_is_asked() {
        let root = configuration(Some("absent"));
        let error = attest(&root, "web", &unreachable()).unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(error.contains("absent.toml"), "{error}");
    }
}
