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
    read_attestation, read_review, CompatibilityAttestation, LiveCompatibilityEvidence,
    MemorySetup, VirshCompatibilityEvidenceSource,
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
    /// The host's `virsh` runner on this connection: as root, the one
    /// connection the process shares ([`Virsh::shared`]).
    pub fn runner(&self) -> Virsh {
        Virsh::shared("virsh", self.timeout, &self.connection)
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
    let (settings, review) = settings_and_review(directory, vm)?;
    let state = libvirt.state(vm)?;
    if state != "running" {
        return Err(format!("{vm} is {state}: a VM is attested while it runs"));
    }
    let live =
        VirshCompatibilityEvidenceSource::new(libvirt.runner(), vm, &settings.alias).collect()?;
    write_attestation(directory, vm, CompatibilityAttestation::new(live, review)?)
}

/// Record a VM that isn't running from the definition it starts with:
/// only for one whose record an earlier RAMpire wrote, so a VM shut off
/// during an upgrade isn't refused every resize once it starts. QEMU's
/// two virtio-mem options can't be read without the VM; they are recorded
/// as on, which every record needs, and every resize holds them against
/// the running VM, so a VM that runs without them is still refused.
fn attest_definition(directory: &Path, vm: &str, xml: &str) -> Result<PathBuf, String> {
    let (settings, review) = settings_and_review(directory, vm)?;
    let memory = MemorySetup::read(xml, &settings.alias)
        .map_err(|error| format!("read {vm}'s definition: {error}"))?;
    let evidence = LiveCompatibilityEvidence {
        vm_name: vm.to_owned(),
        device_alias: settings.alias.clone(),
        memory,
        dynamic_memslots: true,
        unplugged_inaccessible: true,
    };
    write_attestation(
        directory,
        vm,
        CompatibilityAttestation::new(evidence, review)?,
    )
}

/// The VM's settings and the review profile they name, for its record.
fn settings_and_review(
    directory: &Path,
    vm: &str,
) -> Result<(VmSettings, rampire_host::attestation::CompatibilityReview), String> {
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
    Ok((settings, review))
}

fn write_attestation(
    directory: &Path,
    vm: &str,
    document: CompatibilityAttestation,
) -> Result<PathBuf, String> {
    let path = attestation_path(directory, vm);
    write_atomically(
        &path,
        format!(
            "{}
",
            document.to_pretty_json()?
        )
        .as_bytes(),
    )?;
    Ok(path)
}

/// Keep the record of how each VM is set up current, so no person has to:
/// a running VM that names a review profile is recorded when it has no
/// record yet, or when it changed since (libvirt upgraded, its definition
/// edited) and what its review read from the definition still holds. A
/// VM that isn't running is recorded only when an earlier RAMpire wrote
/// its record, from the definition it will start with.
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
        if vm.review.is_none() {
            continue;
        }
        let running = match libvirt.state(name) {
            Ok(state) => state == "running",
            Err(_) => continue,
        };
        // One VM that can't be recorded keeps its old record, which
        // `check` then judges; it doesn't stop the others being recorded.
        let kept = if running {
            keep_one_current(directory, libvirt, ui, &vm)
        } else {
            keep_one_shut_off(directory, libvirt, ui, &vm)
        };
        match kept {
            Ok(true) => recorded.push(vm.vm_name),
            Ok(false) => {}
            Err(error) => ui.say(&format!(
                "{name}: {error}; rampire doctor {name} says what to do"
            )),
        }
    }
    Ok(recorded)
}

/// Record one running VM's setup if it has none or changed safely.
/// Returns whether it was recorded.
fn keep_one_current(
    directory: &Path,
    libvirt: &Libvirt,
    ui: &mut dyn Ui,
    vm: &VmSettings,
) -> Result<bool, String> {
    let name = vm.vm_name.as_str();
    let path = attestation_path(directory, name);
    if path.exists() {
        let live =
            VirshCompatibilityEvidenceSource::new(libvirt.runner(), name, &vm.alias).collect()?;
        let unchanged = read_attestation(&path)
            .is_ok_and(|attestation| attestation.validate_live(&live, vm.guest_os).is_ok());
        if unchanged || !still_safe(libvirt, ui, name)? {
            return Ok(false);
        }
    }
    attest(directory, name, libvirt)?;
    Ok(true)
}

/// Record a VM that isn't running when an earlier RAMpire wrote its
/// record, from its definition. Returns whether it was recorded.
fn keep_one_shut_off(
    directory: &Path,
    libvirt: &Libvirt,
    ui: &mut dyn Ui,
    vm: &VmSettings,
) -> Result<bool, String> {
    let name = vm.vm_name.as_str();
    let path = attestation_path(directory, name);
    let earlier = std::fs::read_to_string(&path)
        .ok()
        .is_some_and(|json| CompatibilityAttestation::other_version(&json).is_some());
    if !earlier {
        return Ok(false);
    }
    let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
    if !definition_safe(&xml, ui, name)? {
        return Ok(false);
    }
    attest_definition(directory, name, &xml)?;
    Ok(true)
}

/// Whether a VM that changed can still be resized: its definition is
/// read again for what the review answered from it. What it can't show
/// is asked; the review's question about the guest's programs is not,
/// since a change to the setup doesn't change them.
fn still_safe(libvirt: &Libvirt, ui: &mut dyn Ui, vm: &str) -> Result<bool, String> {
    definition_safe(&libvirt.virsh(&["dumpxml", vm])?, ui, vm)
}

/// [`still_safe`] for the definition in `xml`.
fn definition_safe(xml: &str, ui: &mut dyn Ui, vm: &str) -> Result<bool, String> {
    let inspection = crate::vm::inspect(xml)?;
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
    shut_down(vm, libvirt, shutdown_timeout)?;
    libvirt.virsh(&["start", vm])?;
    Ok(())
}

/// Shut the VM down and wait for it to stop.
pub fn shut_down(vm: &str, libvirt: &Libvirt, shutdown_timeout: Duration) -> Result<(), String> {
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
            format!("alias = \"m\"\npriority = 1\nguest_os = \"linux\"\nmin_ram = \"1GB\"\nmax_ram = \"4GB\"\n{review}"),
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

    /// A definition with RAMpire's device, as `virsh dumpxml --inactive`
    /// shows it (`live` false) or libvirt shows it running.
    fn definition(live: bool) -> String {
        let (id, current) = if live {
            (
                " id='7'",
                "<current unit='KiB'>0</current><address base='0x100000000'/>",
            )
        } else {
            ("", "")
        };
        format!(
            "<domain type='kvm'{id}><name>web</name>
  <maxMemory slots='1' unit='KiB'>4194304</maxMemory>
  <memory unit='KiB'>4194304</memory>
  <currentMemory unit='KiB'>4194304</currentMemory>
  <cpu><numa><cell id='0' cpus='0-1' memory='524288' unit='KiB'/></numa></cpu>
  <devices>
    <memballoon model='none'/>
    <memory model='virtio-mem'>
      <target><size unit='KiB'>3670016</size><node>0</node><block unit='KiB'>2048</block><requested unit='KiB'>0</requested>{current}</target>
      <alias name='m'/>
    </memory>
  </devices>
</domain>"
        )
    }

    #[test]
    fn a_shut_off_vm_is_recorded_from_the_definition_it_starts_with() {
        let root = configuration(Some("linux-standard"));
        std::fs::create_dir_all(root.join(REVIEW_DIRECTORY)).unwrap();
        std::fs::write(
            root.join(REVIEW_DIRECTORY).join("linux-standard.toml"),
            r#"version = 1
guest_os = "linux"
workload_reviewed = { value = true, source = "confirmed" }
no_vdpa = { value = true, source = "detected" }
no_rdma_migration = { value = true, source = "confirmed" }
no_vfio_nvme = { value = true, source = "detected" }
no_mlock = { value = true, source = "detected" }
no_secure_virtualization = { value = true, source = "detected" }
no_unsupported_vhost_user = { value = true, source = "detected" }
balloon_resize_inactive = { value = true, source = "detected" }
memory_slot_budget = { value = 1, source = "confirmed" }
vfio_mapping_budget = { value = 1, source = "confirmed" }
guest_driver_version = { value = "linux 6.12", source = "confirmed" }
"#,
        )
        .unwrap();
        let path = attest_definition(&root, "web", &definition(false));
        let recorded = path.as_ref().map(|path| read_attestation(path));
        std::fs::remove_dir_all(&root).unwrap();
        let recorded = recorded.unwrap().unwrap();
        // Once it starts, the running VM matches the record: its current
        // size and the device's address aren't part of it.
        let live = LiveCompatibilityEvidence {
            vm_name: "web".to_owned(),
            device_alias: "m".to_owned(),
            memory: MemorySetup::read(&definition(true), "m").unwrap(),
            dynamic_memslots: true,
            unplugged_inaccessible: true,
        };
        assert_eq!(
            recorded.validate_live(&live, settings::GuestOs::Linux),
            Ok(())
        );
        // A VM that runs without QEMU's options is still refused.
        let without = LiveCompatibilityEvidence {
            unplugged_inaccessible: false,
            ..live
        };
        assert!(recorded
            .validate_live(&without, settings::GuestOs::Linux)
            .is_err());
    }

    #[test]
    fn a_missing_profile_is_reported_before_libvirt_is_asked() {
        let root = configuration(Some("absent"));
        let error = attest(&root, "web", &unreachable()).unwrap_err();
        std::fs::remove_dir_all(&root).unwrap();
        assert!(error.contains("absent.toml"), "{error}");
    }
}
