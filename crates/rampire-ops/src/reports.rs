//! The documents `rampire COMMAND --json` writes. Each names
//! its schema and version; docs/json.md describes them, and the tests below
//! hold each to its example in `fixtures/json/`.

use serde::{Deserialize, Serialize};

use rampire_core::{ControlHealth, ControllerStatusSnapshot};

use crate::doctor::Finding;

/// The version of every report below.
pub const REPORT_VERSION: u16 = 1;

rampire_core::schema_name! {
    /// `rampire status --json`.
    pub struct StatusReportSchema = "rampire.status-report";
}

rampire_core::schema_name! {
    /// `rampire check --json`.
    pub struct CheckSchema = "rampire.check";
}

rampire_core::schema_name! {
    /// `rampire doctor --json`.
    pub struct DoctorSchema = "rampire.doctor";
}

rampire_core::schema_name! {
    /// `rampire vm list --json`.
    pub struct VmListSchema = "rampire.vm-list";
}

/// `rampire status --json`: every VM's controller status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusReport {
    pub schema: StatusReportSchema,
    pub version: u16,
    pub controller: ControllerUnit,
    pub vms: Vec<ControllerStatusSnapshot>,
    /// Managed VMs whose status couldn't be read, usually because they
    /// aren't running.
    pub unavailable: Vec<UnavailableVm>,
}

/// A managed VM `status` couldn't read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnavailableVm {
    pub vm_name: String,
    /// libvirt's state for it (`shut off`, `paused`, ...), or `unknown`.
    pub state: String,
    /// Why its status couldn't be read.
    pub reason: String,
}

/// The controller's systemd unit, as `systemctl show` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ControllerUnit {
    pub unit: String,
    /// `active`, `inactive`, `failed`, ...
    pub active_state: String,
    /// `running`, `dead`, ...
    pub sub_state: String,
    /// `enabled`, `disabled`, ...
    pub unit_file_state: String,
}

/// `rampire check --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckReport {
    pub schema: CheckSchema,
    pub version: u16,
    pub passed: bool,
    pub vms: Vec<CheckedVm>,
}

/// One VM in a [`CheckReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckedVm {
    pub vm_name: String,
    /// `None` when the VM passed.
    pub problem: Option<String>,
}

/// `rampire doctor --json`, for the host or for one VM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DoctorReport {
    pub schema: DoctorSchema,
    pub version: u16,
    /// The VM checked, for `rampire doctor VM`; absent for the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vm: Option<String>,
    pub passed: bool,
    pub findings: Vec<Finding>,
}

/// `rampire vm list --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VmListReport {
    pub schema: VmListSchema,
    pub version: u16,
    pub vms: Vec<ListedVm>,
}

/// What RAMpire does with a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Management {
    /// The controller runs it.
    Managed,
    /// `vm add` wrote its settings but didn't apply them.
    Unfinished,
    NotManaged,
}

impl Management {
    pub fn label(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::Unfinished => "unfinished",
            Self::NotManaged => "not managed",
        }
    }
}

/// A problem, with its fix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Problem {
    pub problem: String,
    pub fix: String,
}

/// Whether a guest service has published.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Published {
    /// A publication was read: it is this old, in seconds.
    #[serde(rename = "published")]
    Yes { age_seconds: u64 },
    /// The guest was asked and has no publication; for a VM that isn't
    /// managed, the guest service isn't installed.
    #[serde(rename = "not_published")]
    No,
    /// A VM that isn't managed has the guest service installed. It
    /// publishes once RAMpire manages the VM and sends its settings.
    Installed,
    /// The guest couldn't be asked: not running, or no guest agent.
    Unknown,
}

/// One VM in a [`VmListReport`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListedVm {
    pub vm_name: String,
    /// libvirt's state: `running`, `shut off`, ...; `missing` for a managed
    /// VM libvirt no longer has.
    pub state: String,
    pub management: Management,
    /// A managed VM's status, when it could be read.
    pub status: Option<ControllerStatusSnapshot>,
    /// One line about it for a person.
    pub detail: String,
    /// For a VM that isn't managed: what `vm add` would change.
    pub to_fix: Vec<String>,
    /// For a VM that isn't managed: what would still stop `vm add`.
    pub problems: Vec<Problem>,
    /// Whether its guest service has published.
    pub guest: Published,
    /// The system in the VM as its definition's libosinfo metadata names
    /// it, such as `http://debian.org/debian/13`, when it does.
    pub os_id: Option<String>,
    /// For a VM that isn't managed: the sizes `vm add` would give it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested: Option<SuggestedSizes>,
    /// For a VM that isn't managed: its definition still has RAMpire's
    /// virtio-mem device or control channel, which `vm remove` takes out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rampire_parts: bool,
    /// For a VM that isn't managed: adding it while it runs shuts it down
    /// and starts it again, since its changes take effect at its next
    /// start.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub add_restarts: bool,
    /// For a managed VM that runs: it doesn't run its saved definition,
    /// which `vm restart` brings in.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub runs_behind: bool,
}

/// The sizes `vm add` suggests for a VM, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestedSizes {
    pub min_ram: u64,
    pub max_ram: u64,
    pub free_ram: u64,
}

/// One VM's status in a line a person can read, as `rampire status`
/// prints it.
pub fn status_line(status: &ControllerStatusSnapshot) -> String {
    let state = if status.actuation_latched {
        format!(
            "latched: {}",
            status
                .latch_reason
                .as_deref()
                .unwrap_or("no reason recorded")
        )
    } else {
        match status.control_health {
            ControlHealth::Converged => "steady".to_owned(),
            ControlHealth::Growing => "growing".to_owned(),
            ControlHealth::Shrinking => "shrinking".to_owned(),
            ControlHealth::RecoveryRequired => "needs recovery: see rampire recover".to_owned(),
            ControlHealth::Latched => "latched".to_owned(),
        }
    };
    let guest = match &status.guest_input.error {
        Some(error) => format!("; guest: {error}"),
        None => String::new(),
    };
    format!(
        "{}: {} of at most {}, {state}{}{guest}",
        status.vm_name,
        size(status.current_bytes),
        size(status.maximum_target_bytes),
        if status.observe_only {
            " (observe only)"
        } else {
            ""
        },
    )
}

/// A size as a person writes it: MiB below a GiB, else GiB to one place.
pub fn size(bytes: u64) -> String {
    use rampire_core::{GIB, MIB};
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use rampire_core::{
        CommandOwnership, ControlHealth, GuestInputStatus, RecoveryState, StatusSchema,
        CONTROLLER_STATUS_VERSION,
    };
    use serde::de::DeserializeOwned;

    use super::*;

    use rampire_core::GIB;

    #[test]
    fn sizes_read_as_a_person_would_write_them() {
        assert_eq!(size(512 << 20), "512 MiB");
        assert_eq!(size(3 << 30), "3.0 GiB");
        assert_eq!(size((3 << 30) + (512 << 20)), "3.5 GiB");
    }

    fn fixtures() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/json")
    }

    fn status_report() -> StatusReport {
        StatusReport {
            schema: StatusReportSchema,
            version: REPORT_VERSION,
            controller: ControllerUnit {
                unit: "rampire.service".to_owned(),
                active_state: "active".to_owned(),
                sub_state: "running".to_owned(),
                unit_file_state: "enabled".to_owned(),
            },
            vms: vec![ControllerStatusSnapshot {
                schema: StatusSchema,
                version: CONTROLLER_STATUS_VERSION,
                observed_unix_millis: 1_794_600_000_000,
                vm_name: "web".to_owned(),
                device_alias: "ua-virtiomem0".to_owned(),
                device_size_bytes: 15 * GIB,
                block_size_bytes: 2 << 20,
                maximum_target_bytes: 15 * GIB,
                requested_bytes: GIB,
                current_bytes: GIB,
                observe_only: false,
                guest_input: GuestInputStatus {
                    error: None,
                    observed_unix_millis: Some(1_794_599_999_500),
                    request: None,
                },
                admitted_request: None,
                result: None,
                command_ownership: CommandOwnership::None,
                command: None,
                control_health: ControlHealth::Converged,
                actuation_latched: false,
                latch_reason: None,
                recovery_state: RecoveryState::NotRequired,
                recovery_reason: None,
                last_latch_clear_reason: None,
                compatibility_fingerprint_sha256: "e".repeat(64),
            }],
            unavailable: vec![UnavailableVm {
                vm_name: "spare".to_owned(),
                state: "shut off".to_owned(),
                reason: "virtio-mem XML is missing current".to_owned(),
            }],
        }
    }

    fn check_report() -> CheckReport {
        CheckReport {
            schema: CheckSchema,
            version: REPORT_VERSION,
            passed: false,
            vms: vec![
                CheckedVm {
                    vm_name: "web".to_owned(),
                    problem: None,
                },
                CheckedVm {
                    vm_name: "db".to_owned(),
                    problem: Some("live compatibility: drifted: domain_xml_sha256".to_owned()),
                },
            ],
        }
    }

    fn doctor_report() -> DoctorReport {
        DoctorReport {
            schema: DoctorSchema,
            version: REPORT_VERSION,
            vm: None,
            passed: false,
            findings: vec![
                Finding {
                    check: "QEMU".to_owned(),
                    ok: true,
                    detail: "10.1.0".to_owned(),
                    fix: None,
                    run: None,
                },
                Finding {
                    check: "service account".to_owned(),
                    ok: false,
                    detail: "there is no user rampire".to_owned(),
                    fix: Some(
                        "useradd --system --no-create-home --shell /sbin/nologin rampire"
                            .to_owned(),
                    ),
                    run: None,
                },
            ],
        }
    }

    fn json<T: Serialize>(document: &T) -> String {
        format!("{}\n", serde_json::to_string_pretty(document).unwrap())
    }

    fn matches_fixture<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(
        name: &str,
        document: &T,
    ) {
        let path = fixtures().join(name);
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}; write it with --ignored", path.display()));
        assert_eq!(
            text,
            json(document),
            "{name} changed: update docs/json.md too"
        );
        assert_eq!(&serde_json::from_str::<T>(&text).unwrap(), document);
    }

    fn vm_list_report() -> VmListReport {
        VmListReport {
            schema: VmListSchema,
            version: REPORT_VERSION,
            vms: vec![
                ListedVm {
                    vm_name: "db".to_owned(),
                    state: "running".to_owned(),
                    management: Management::NotManaged,
                    status: None,
                    detail: "published 4s ago".to_owned(),
                    to_fix: vec![
                        "add the control channel org.rampire.control.0".to_owned(),
                        "add a 6144 MiB virtio-mem device".to_owned(),
                    ],
                    problems: vec![],
                    guest: Published::Yes { age_seconds: 4 },
                    os_id: Some("http://debian.org/debian/13".to_owned()),
                    suggested: None,
                    rampire_parts: false,
                    add_restarts: false,
                    runs_behind: false,
                },
                ListedVm {
                    vm_name: "web".to_owned(),
                    state: "running".to_owned(),
                    management: Management::Managed,
                    detail: "web: 1.0 GiB of at most 15.0 GiB, steady".to_owned(),
                    status: status_report().vms.into_iter().next(),
                    to_fix: vec![],
                    problems: vec![],
                    guest: Published::Yes { age_seconds: 0 },
                    os_id: Some("http://redhat.com/rhel/10.0".to_owned()),
                    suggested: None,
                    rampire_parts: false,
                    add_restarts: false,
                    runs_behind: false,
                },
            ],
        }
    }

    #[test]
    fn every_report_matches_its_documented_example() {
        matches_fixture("status-report.json", &status_report());
        matches_fixture("check.json", &check_report());
        matches_fixture("doctor.json", &doctor_report());
        matches_fixture("vm-list.json", &vm_list_report());
    }

    #[test]
    fn a_report_of_another_schema_is_refused() {
        let text = json(&check_report()).replace("rampire.check", "rampire.doctor");
        assert!(serde_json::from_str::<CheckReport>(&text).is_err());
    }

    /// Write the examples after a deliberate change to a report.
    #[test]
    #[ignore]
    fn write_the_examples() {
        std::fs::create_dir_all(fixtures()).unwrap();
        for (name, text) in [
            ("status-report.json", json(&status_report())),
            ("check.json", json(&check_report())),
            ("doctor.json", json(&doctor_report())),
            ("vm-list.json", json(&vm_list_report())),
        ] {
            std::fs::write(fixtures().join(name), text).unwrap();
        }
    }
}
