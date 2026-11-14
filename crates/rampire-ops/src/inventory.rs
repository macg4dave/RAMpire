//! `rampire vm list`: every VM on the libvirt connection, and what RAMpire
//! does with it: managed, parked by `vm remove`, left half-added by a `vm
//! add` that didn't finish, or not managed. For a VM that isn't managed it
//! says what `vm add --fix-xml` would change, what would still stop it, and
//! whether its guest service has published.

use std::collections::BTreeSet;
use std::path::Path;

use rampire_core::settings::LINUX_CONTROL_PATH;
use rampire_core::RawTelemetryEnvelope;
use rampire_host::config::HostSettings;
use rampire_host::qga::{GuestFileReader, VirshGuestFiles};

use crate::context::{Context, PARKED_DIRECTORY};
use crate::host_readings::MAX_TELEMETRY_BYTES;
use crate::reports::{ListedVm, Management, Problem, Published, VmListReport, VmListSchema};

/// The names of the TOML files in `directory`, without `.toml`.
pub fn toml_names(directory: &Path) -> BTreeSet<String> {
    std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_suffix(".toml")
                .map(str::to_owned)
        })
        .collect()
}

/// What the configuration says about VMs: those the controller runs, those
/// parked in the draft, and those with settings only in the draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    pub managed: BTreeSet<String>,
    pub parked: BTreeSet<String>,
    pub unfinished: BTreeSet<String>,
}

impl Known {
    pub fn read(context: &Context) -> Result<Self, String> {
        let running = context.running();
        let managed: BTreeSet<String> =
            if running.join(rampire_core::settings::GLOBAL_FILE).exists() {
                HostSettings::load(&running)
                    .map_err(|error| error.to_string())?
                    .vms
                    .into_iter()
                    .map(|vm| vm.vm_name)
                    .collect()
            } else {
                BTreeSet::new()
            };
        let draft = context.draft();
        let unfinished = toml_names(&draft.join(rampire_core::settings::VMS_DIRECTORY))
            .into_iter()
            .filter(|name| !managed.contains(name))
            .collect();
        Ok(Self {
            parked: toml_names(&draft.join(PARKED_DIRECTORY)),
            managed,
            unfinished,
        })
    }

    pub fn management(&self, name: &str) -> Management {
        if self.managed.contains(name) {
            Management::Managed
        } else if self.unfinished.contains(name) {
            Management::Unfinished
        } else if self.parked.contains(name) {
            Management::Parked
        } else {
            Management::NotManaged
        }
    }
}

/// The system in a domain as its libosinfo metadata names it.
pub fn libosinfo_id(xml: &str) -> Option<String> {
    let start = xml.find("<libosinfo:os ")?;
    let element = &xml[start..start + xml[start..].find('>')?];
    let at = element.find("id=")? + "id=".len();
    let quote = element[at..].chars().next()?;
    let rest = &element[at + 1..];
    Some(rest[..rest.find(quote)?].to_owned())
}

/// Every domain on the connection, by name.
pub fn domains(libvirt: &crate::attest::Libvirt) -> Result<Vec<String>, String> {
    let mut names: Vec<String> = libvirt
        .virsh(&["list", "--all", "--name"])?
        .lines()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect();
    names.sort();
    Ok(names)
}

/// What `vm add --fix-xml` would change in a definition, and the problems
/// that would still stop RAMpire managing it afterwards.
pub fn add_preview(xml: &str) -> Result<(Vec<String>, Vec<Problem>), String> {
    let inspection = crate::vm::inspect(xml)?;
    let to_fix = inspection.fixable();
    let after = match crate::vm::fix_xml(xml, &inspection)? {
        Some(fixed) => crate::vm::inspect(&fixed)?,
        None => inspection,
    };
    let problems = after
        .problems()
        .into_iter()
        .map(|(problem, fix)| Problem { problem, fix })
        .collect();
    Ok((to_fix, problems))
}

/// Read a guest's publication from its own file through the guest agent:
/// the route that works before RAMpire manages the VM.
pub fn read_guest_publication(
    libvirt: &crate::attest::Libvirt,
    vm: &str,
) -> (Published, Option<RawTelemetryEnvelope>, String) {
    let files = VirshGuestFiles::new(libvirt.runner(), vm);
    match files.read_file(LINUX_CONTROL_PATH, MAX_TELEMETRY_BYTES) {
        Ok(bytes) => match serde_json::from_slice::<RawTelemetryEnvelope>(&bytes) {
            Ok(envelope) => {
                let now = crate::process::unix_millis().unwrap_or(0);
                let age_seconds = now.saturating_sub(envelope.observed_unix_millis) / 1000;
                (
                    Published::Yes { age_seconds },
                    Some(envelope),
                    format!("published {age_seconds}s ago"),
                )
            }
            Err(error) => (
                Published::No,
                None,
                format!("{LINUX_CONTROL_PATH} isn't a publication: {error}"),
            ),
        },
        // A guest publishes only once a host gives it settings, so before
        // RAMpire manages the VM its unit file says it is there.
        Err(error) if is_missing_file(&error) => {
            match files.read_file(GUEST_UNIT, MAX_TELEMETRY_BYTES) {
                Ok(_) => (
                    Published::Installed,
                    None,
                    "its guest service is installed; it publishes once RAMpire manages the VM"
                        .to_owned(),
                ),
                Err(error) if is_missing_file(&error) => (
                    Published::No,
                    None,
                    "its guest service isn't installed: install rampire-guest in the VM".to_owned(),
                ),
                Err(error) => (
                    Published::Unknown,
                    None,
                    format!("the guest agent didn't answer: {error}"),
                ),
            }
        }
        Err(error) => (
            Published::Unknown,
            None,
            format!("the guest agent didn't answer: {error}"),
        ),
    }
}

/// The guest service's unit, as the guest package installs it.
const GUEST_UNIT: &str = "/usr/lib/systemd/system/rampire-guest.service";

fn is_missing_file(error: &str) -> bool {
    error.contains("No such file") || error.contains("cannot find the file")
}

/// `rampire vm list`.
pub fn list(context: &Context) -> Result<VmListReport, String> {
    let libvirt = context.libvirt()?;
    let known = Known::read(context)?;
    let mut vms = Vec::new();
    for name in domains(&libvirt)? {
        vms.push(listed(context, &libvirt, &known, &name));
    }
    // A managed VM libvirt no longer has is listed too, so it can be removed.
    for name in &known.managed {
        if !vms.iter().any(|vm: &ListedVm| &vm.vm_name == name) {
            vms.push(ListedVm {
                vm_name: name.clone(),
                state: "missing".to_owned(),
                management: Management::Managed,
                status: None,
                detail: format!(
                    "libvirt has no domain {name}; stop managing it with: rampire vm remove {name}"
                ),
                to_fix: Vec::new(),
                problems: Vec::new(),
                guest: Published::Unknown,
                os_id: None,
            });
        }
    }
    Ok(VmListReport {
        schema: VmListSchema,
        version: crate::reports::REPORT_VERSION,
        vms,
    })
}

fn listed(
    context: &Context,
    libvirt: &crate::attest::Libvirt,
    known: &Known,
    name: &str,
) -> ListedVm {
    let state = libvirt.state(name).unwrap_or_else(|_| "unknown".to_owned());
    let management = known.management(name);
    let mut vm = ListedVm {
        vm_name: name.to_owned(),
        state: state.clone(),
        management,
        status: None,
        detail: String::new(),
        to_fix: Vec::new(),
        problems: Vec::new(),
        guest: Published::Unknown,
        os_id: libvirt
            .virsh(&["dumpxml", "--inactive", name])
            .ok()
            .and_then(|xml| libosinfo_id(&xml)),
    };
    match management {
        Management::Managed => match read_status(context, name) {
            Ok(status) => {
                vm.detail = crate::reports::status_line(&status);
                vm.guest = match status.guest_input.observed_unix_millis {
                    Some(observed) => Published::Yes {
                        age_seconds: status.observed_unix_millis.saturating_sub(observed) / 1000,
                    },
                    None => Published::No,
                };
                vm.status = Some(status);
            }
            Err(reason) => vm.detail = format!("{state}, so its status can't be read ({reason})"),
        },
        Management::Parked => {
            vm.detail = format!("parked: rampire vm add {name} brings it back as it was");
        }
        Management::Unfinished => {
            vm.detail =
                format!("an earlier vm add didn't finish: rampire vm add {name} finishes it");
        }
        Management::NotManaged => {
            match libvirt
                .virsh(&["dumpxml", "--inactive", name])
                .and_then(|xml| add_preview(&xml))
            {
                Ok((to_fix, problems)) => {
                    vm.to_fix = to_fix;
                    vm.problems = problems;
                }
                Err(error) => vm.problems.push(Problem {
                    problem: format!("its definition couldn't be read: {error}"),
                    fix: "check the domain with virsh dumpxml".to_owned(),
                }),
            }
            if state == "running" {
                let (published, _, detail) = read_guest_publication(libvirt, name);
                vm.guest = published;
                vm.detail = detail;
            } else {
                vm.detail = format!("{state}: start it to see whether its guest service runs");
            }
        }
    }
    vm
}

/// A managed VM's status, as `rampire status VM` reads it.
pub fn read_status(
    context: &Context,
    name: &str,
) -> Result<rampire_core::ControllerStatusSnapshot, String> {
    let mut bytes = Vec::new();
    rampire_host::operator_actions::status(&context.target(Some(name))?, &mut bytes)?;
    rampire_core::parse_controller_status(&bytes).map_err(|error| error.to_string())
}

/// `vm list` as text for a person.
pub fn text(report: &VmListReport) -> String {
    if report.vms.is_empty() {
        return "libvirt has no VMs on this connection\n".to_owned();
    }
    let width = report
        .vms
        .iter()
        .map(|vm| vm.vm_name.chars().count())
        .max()
        .unwrap_or(0)
        .max(2);
    let mut out = String::new();
    for vm in &report.vms {
        out.push_str(&format!(
            "{:<width$}  {:<11}  {:<8}  {}\n",
            vm.vm_name,
            vm.management.label(),
            vm.state,
            vm.detail
        ));
        if vm.management == Management::NotManaged {
            for change in &vm.to_fix {
                out.push_str(&format!("{:width$}  to fix: {change}\n", ""));
            }
            for problem in &vm.problems {
                out.push_str(&format!(
                    "{:width$}  problem: {}\n{:width$}    fix: {}\n",
                    "", problem.problem, "", problem.fix
                ));
            }
            if vm.problems.is_empty() {
                out.push_str(&format!(
                    "{:width$}  add it with: rampire vm add {} --fix-xml\n",
                    "", vm.vm_name
                ));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = "<domain type='kvm'>
  <name>web</name>
  <memory unit='KiB'>2097152</memory>
  <vcpu>2</vcpu>
  <devices>
    <memballoon model='virtio'/>
  </devices>
</domain>
";

    #[test]
    fn a_plain_vm_shows_every_change_fix_xml_makes_and_nothing_left_to_stop_it() {
        let (to_fix, problems) = add_preview(PLAIN).unwrap();
        assert!(to_fix
            .iter()
            .any(|change| change.contains("control channel")));
        assert!(to_fix
            .iter()
            .any(|change| change.contains("virtio-mem device")));
        assert!(to_fix.iter().any(|change| change.contains("balloon")));
        assert_eq!(problems, []);
    }

    #[test]
    fn the_system_comes_from_the_libosinfo_metadata() {
        let xml = "<domain><metadata><libosinfo:libosinfo xmlns:libosinfo=\"http://libosinfo.org/xmlns/libvirt/domain/1.0\"><libosinfo:os id=\"http://debian.org/debian/13\"/></libosinfo:libosinfo></metadata></domain>";
        assert_eq!(
            libosinfo_id(xml).as_deref(),
            Some("http://debian.org/debian/13")
        );
        assert_eq!(libosinfo_id(PLAIN), None);
    }

    #[test]
    fn vms_are_managed_parked_unfinished_or_not_managed() {
        let known = Known {
            managed: ["web".to_owned()].into(),
            parked: ["old".to_owned(), "both".to_owned()].into(),
            unfinished: ["both".to_owned()].into(),
        };
        assert_eq!(known.management("web"), Management::Managed);
        assert_eq!(known.management("old"), Management::Parked);
        assert_eq!(known.management("both"), Management::Unfinished);
        assert_eq!(known.management("new"), Management::NotManaged);
    }

    #[test]
    fn the_text_says_how_to_add_a_vm_nothing_stops() {
        let report = VmListReport {
            schema: VmListSchema,
            version: 1,
            vms: vec![ListedVm {
                vm_name: "web".to_owned(),
                state: "running".to_owned(),
                management: Management::NotManaged,
                status: None,
                detail: "published 3s ago".to_owned(),
                to_fix: vec!["add a 6144 MiB virtio-mem device".to_owned()],
                problems: vec![],
                guest: Published::Yes { age_seconds: 3 },
                os_id: None,
            }],
        };
        assert_eq!(
            text(&report),
            "web  not managed  running   published 3s ago\n     to fix: add a 6144 MiB virtio-mem device\n     add it with: rampire vm add web --fix-xml\n"
        );
    }
}
