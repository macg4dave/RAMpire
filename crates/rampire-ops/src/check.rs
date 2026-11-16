//! `rampire check`: validate the configuration and every VM against the
//! setup RAMpire recorded for it. It reads; it changes nothing.

use std::path::Path;

use rampire_host::attestation::{read_attestation, AttestedCompatibilitySource};
use rampire_host::compatibility_source::CompatibilitySource;
use rampire_host::config::{HostConfig, HostSettings};
use rampire_host::virsh::{Virsh, VirshCommand, VirshError};

use crate::reports::{CheckReport, CheckSchema, CheckedVm};

/// How `check` says a VM's domain is gone.
const NO_DOMAIN: &str = "libvirt has no domain";

/// Load the configuration in `directory` and check every VM. A
/// configuration that doesn't load is an error; a VM that fails is a
/// problem in the report, so every VM is checked.
pub fn run(directory: &Path) -> Result<CheckReport, String> {
    let settings = HostSettings::load(directory).map_err(|error| error.to_string())?;
    let vms: Vec<CheckedVm> = settings
        .vms
        .iter()
        .map(|config| CheckedVm {
            vm_name: config.vm_name.clone(),
            problem: check_vm(config).err(),
        })
        .collect();
    Ok(CheckReport {
        schema: CheckSchema,
        version: crate::reports::REPORT_VERSION,
        passed: vms.iter().all(|vm| vm.problem.is_none()),
        vms,
    })
}

/// The command that fixes a VM's problem, as `check`, `apply` and the
/// doctor give it.
pub fn fix(vm: &str, problem: &str) -> String {
    format!("rampire {}", fix_words(vm, problem).join(" "))
}

/// [`fix`]'s command, as its words after `rampire`.
pub fn fix_words(vm: &str, problem: &str) -> Vec<String> {
    if problem.contains(NO_DOMAIN) {
        vec!["vm".to_owned(), "remove".to_owned(), vm.to_owned()]
    } else {
        // `apply` checks the VM again and records it, or says why it can't.
        vec!["apply".to_owned()]
    }
}

/// The report as `check` prints it: a line per VM, each problem with its
/// fix. With `problems_only`, as `apply` shows a draft that fails.
pub fn text(report: &CheckReport, problems_only: bool) -> String {
    let mut text = String::new();
    if report.vms.is_empty() && !problems_only {
        text.push_str("ok: the configuration is valid and manages no VMs\n");
    }
    for vm in &report.vms {
        match &vm.problem {
            None if problems_only => {}
            None => text.push_str(&format!("ok: {}\n", vm.vm_name)),
            Some(problem) => text.push_str(&format!(
                "problem: {}: {problem}\n  fix: {}\n",
                vm.vm_name,
                fix(&vm.vm_name, problem)
            )),
        }
    }
    text
}

fn check_vm(config: &HostConfig) -> Result<(), String> {
    let virsh = Virsh::with_connection(
        &config.virsh_binary,
        config.command_timeout,
        &config.libvirt_uri,
    );
    check_domain(&virsh, config)
}

/// Check one VM through `virsh`. A VM with no QEMU process (shut off or
/// crashed) has no live evidence to compare: its record must still read,
/// and the controller compares it with the live VM before it resizes it.
fn check_domain(virsh: &impl VirshCommand, config: &HostConfig) -> Result<(), String> {
    let Some(state) = domain_state(virsh, &config.vm_name)? else {
        return Err(format!("{NO_DOMAIN} {}", config.vm_name));
    };
    if matches!(state.as_str(), "shut off" | "crashed") {
        return read_attestation(Path::new(&config.compatibility_attestation_path)).map(drop);
    }
    // Reads the attestation, then compares it with the live evidence.
    AttestedCompatibilitySource::new(
        virsh,
        &config.vm_name,
        &config.alias,
        config.guest_os,
        &config.compatibility_attestation_path,
    )
    .compatibility()?
    .validate_for_resize()
    .map_err(|error| format!("live compatibility: {error}"))
}

/// The VM's state as `virsh domstate` names it, or `None` when libvirt
/// doesn't know it. Any other failure to ask is an error.
fn domain_state(virsh: &impl VirshCommand, vm: &str) -> Result<Option<String>, String> {
    match virsh.run(&["domstate".to_owned(), vm.to_owned()]) {
        Ok(state) => Ok(Some(state.trim().to_owned())),
        Err(VirshError::Failed { stderr, .. }) if is_missing_domain(&stderr) => Ok(None),
        Err(error) => Err(format!("ask libvirt about the domain: {error}")),
    }
}

/// libvirt's answer for a name it doesn't know.
fn is_missing_domain(stderr: &str) -> bool {
    stderr.contains("failed to get domain") || stderr.contains("Domain not found")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_libvirts_answer_for_an_unknown_domain() {
        assert!(is_missing_domain(
            "error: failed to get domain 'gone'
error: Domain not found: no domain with matching name 'gone'"
        ));
        assert!(!is_missing_domain(
            "error: failed to connect to the hypervisor"
        ));
    }

    #[test]
    fn an_empty_configuration_passes() {
        let root = std::env::temp_dir().join(format!("rampire-check-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("vms.d")).unwrap();
        let report = run(&root);
        std::fs::remove_dir_all(&root).unwrap();
        let report = report.expect("loads");
        assert!(report.vms.is_empty());
        assert!(report.passed);
        assert!(text(&report, false).starts_with("ok: "));
        assert_eq!(text(&report, true), "");
    }

    #[test]
    fn each_problem_names_its_fix() {
        let checked = |vm: &str, problem: Option<&str>| CheckedVm {
            vm_name: vm.to_owned(),
            problem: problem.map(str::to_owned),
        };
        let report = CheckReport {
            schema: CheckSchema,
            version: 1,
            passed: false,
            vms: vec![
                checked("web", None),
                checked(
                    "db",
                    Some("libvirt was upgraded since RAMpire last checked this VM"),
                ),
                checked("old", Some(&format!("{NO_DOMAIN} old"))),
            ],
        };
        assert_eq!(
            text(&report, false),
            "ok: web\n\
             problem: db: libvirt was upgraded since RAMpire last checked this VM\n  \
             fix: rampire apply\n\
             problem: old: libvirt has no domain old\n  fix: rampire vm remove old\n"
        );
        assert!(!text(&report, true).contains("ok: web"));
    }

    /// libvirt with one VM in `state`, which refuses QMP as libvirt does
    /// for a VM that isn't running.
    struct OneVm {
        state: &'static str,
    }

    impl VirshCommand for OneVm {
        fn run(&self, arguments: &[String]) -> Result<String, VirshError> {
            match arguments.first().map(String::as_str) {
                Some("domstate") => Ok(format!("{}\n", self.state)),
                _ => Err(VirshError::Failed {
                    status: "exit status: 1".to_owned(),
                    stderr: "error: Requested operation is not valid: domain is not running"
                        .to_owned(),
                }),
            }
        }
    }

    #[test]
    fn a_shut_off_vm_passes_on_its_record_alone() {
        use rampire_host::attestation::{
            read_review, CompatibilityAttestation, LiveCompatibilityEvidence,
        };
        let root = std::env::temp_dir().join(format!("rampire-check-off-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/config");
        let mut config = HostSettings::load(&fixture)
            .expect("fixture loads")
            .vms
            .remove(0);
        config.compatibility_attestation_path = root.join("vm.json").display().to_string();

        // Without its record, a VM that is off still fails.
        let missing = check_domain(&OneVm { state: "shut off" }, &config);

        let answer =
            |value: &str, source: &str| format!("{{ value = {value}, source = \"{source}\" }}");
        let review_path = root.join("review.toml");
        std::fs::write(
            &review_path,
            format!(
                "version = 1\nguest_os = \"{}\"\nworkload_reviewed = {}\n{}memory_slot_budget = {}\nvfio_mapping_budget = {}\nguest_driver_version = {}\n",
                match config.guest_os {
                    rampire_core::settings::GuestOs::Linux => "linux",
                    rampire_core::settings::GuestOs::Windows => "windows",
                },
                answer("true", "confirmed"),
                [
                    "no_vdpa",
                    "no_rdma_migration",
                    "no_vfio_nvme",
                    "no_mlock",
                    "no_secure_virtualization",
                    "no_unsupported_vhost_user",
                    "balloon_resize_inactive",
                ]
                .iter()
                .map(|key| format!("{key} = {}\n", answer("true", "detected")))
                .collect::<String>(),
                answer("1", "confirmed"),
                answer("1", "confirmed"),
                answer("\"driver 1\"", "confirmed"),
            ),
        )
        .unwrap();
        let live = LiveCompatibilityEvidence {
            vm_name: config.vm_name.clone(),
            device_alias: config.alias.clone(),
            domain_xml_sha256: "a".repeat(64),
            qemu_argv_sha256: "b".repeat(64),
            libvirt_version_sha256: "c".repeat(64),
            qemu_version: serde_json::json!({"qemu": {"major": 9, "minor": 2, "micro": 0}, "package": "qemu-9.2"}),
            dynamic_memslots: true,
            unplugged_inaccessible: true,
        };
        let document =
            CompatibilityAttestation::new(live, read_review(&review_path).expect("review"))
                .expect("attestation");
        std::fs::write(
            &config.compatibility_attestation_path,
            document.to_pretty_json().unwrap(),
        )
        .unwrap();

        let off = check_domain(&OneVm { state: "shut off" }, &config);
        let crashed = check_domain(&OneVm { state: "crashed" }, &config);
        // A running VM is still compared with its live evidence.
        let running = check_domain(&OneVm { state: "running" }, &config);
        std::fs::remove_dir_all(&root).unwrap();

        assert!(missing.unwrap_err().contains("attestation"));
        assert_eq!(off, Ok(()));
        assert_eq!(crashed, Ok(()));
        assert!(running.unwrap_err().contains("domain is not running"));
    }

    #[test]
    fn a_vm_that_fails_is_a_problem_not_an_error() {
        // The fixture VM isn't a libvirt domain here and has no attestation:
        // whichever check meets it first, it fails as a problem in the report.
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/config");
        let report = run(&fixture).expect("fixture loads");
        assert_eq!(report.vms.len(), 1);
        assert!(!report.passed);
        assert!(report.vms[0].problem.is_some());
    }
}
