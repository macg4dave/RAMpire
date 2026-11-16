//! `rampire vm list`: every VM on the libvirt connection, and what RAMpire
//! does with it: managed, left half-added by a `vm add` that didn't
//! finish, or not managed. For a VM that isn't managed it says what
//! `vm add` would change, what would still stop it, and
//! whether its guest service has published.

use std::collections::BTreeSet;
use std::path::Path;

use rampire_core::settings::LINUX_CONTROL_PATH;
use rampire_core::RawTelemetryEnvelope;
use rampire_host::config::HostSettings;
use rampire_host::qga::{GuestFileReader, VirshGuestFiles};

use crate::context::Context;
use crate::host_readings::MAX_TELEMETRY_BYTES;
use crate::reports::{
    ListedVm, Management, Problem, Published, SuggestedSizes, VmListReport, VmListSchema,
};

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

/// What the configuration says about VMs: those the controller runs, and
/// those with settings only in the draft.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Known {
    pub managed: BTreeSet<String>,
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
            .filter_map(|stem| rampire_core::settings::vm_name_from_stem(&stem))
            .filter(|name| !managed.contains(name))
            .collect();
        Ok(Self {
            managed,
            unfinished,
        })
    }

    pub fn management(&self, name: &str) -> Management {
        if self.managed.contains(name) {
            Management::Managed
        } else if self.unfinished.contains(name) {
            Management::Unfinished
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

/// What `vm add` would change in a definition, and the problems
/// that would still stop RAMpire managing it afterwards.
pub fn add_preview(xml: &str) -> Result<(Vec<String>, Vec<Problem>, bool), String> {
    let preview = preview(xml)?;
    Ok((preview.to_fix, preview.problems, preview.parts))
}

/// What `vm add` would do to a definition.
struct Preview {
    /// What it would change.
    to_fix: Vec<String>,
    /// What would still stop it.
    problems: Vec<Problem>,
    /// The sizes it would suggest.
    suggested: Option<SuggestedSizes>,
    /// The definition has RAMpire's device or channel already.
    parts: bool,
    /// Its changes take effect at the VM's next start.
    restarts: bool,
}

/// [`add_preview`], with the sizes `vm add` would suggest.
fn preview(xml: &str) -> Result<Preview, String> {
    let inspection = crate::vm::inspect(xml)?;
    let to_fix = inspection.fixable();
    let parts = inspection.control_channel || inspection.device().is_some();
    let restarts = inspection.fixes_take_a_restart();
    let after = match crate::vm::fix_xml(xml, &inspection)? {
        Some(fixed) => crate::vm::inspect(&fixed)?,
        None => inspection,
    };
    let suggested = crate::vm::suggest(&after, true).map(|sizes| SuggestedSizes {
        min_ram: sizes.min_ram,
        max_ram: sizes.max_ram,
        free_ram: sizes.free_ram,
    });
    let problems = after
        .problems()
        .into_iter()
        .map(|(problem, fix)| Problem { problem, fix })
        .collect();
    Ok(Preview {
        to_fix,
        problems,
        suggested,
        parts,
        restarts,
    })
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
            // The package's unit, else a local install's.
            let unit = files
                .read_file(GUEST_UNIT, MAX_TELEMETRY_BYTES)
                .or_else(|error| match is_missing_file(&error) {
                    true => files.read_file(crate::guest::LOCAL_UNIT, MAX_TELEMETRY_BYTES),
                    false => Err(error),
                });
            match unit {
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

/// How long a VM's guest probe is kept: each probe of a guest without
/// RAMpire's files is a failed guest-agent call, which libvirt logs as an
/// error, so the TUI's list doesn't ask every time it reads.
const PROBE_KEPT: std::time::Duration = std::time::Duration::from_secs(300);

/// Each VM's last guest probe: its domain ID then, when, and what it said.
type Probes = std::collections::HashMap<String, (String, std::time::Instant, Published, String)>;

static PROBES: std::sync::Mutex<Option<Probes>> = std::sync::Mutex::new(None);

/// A VM's guest probe, kept for [`PROBE_KEPT`] while the domain runs as
/// the same ID: a VM started again, as after a guest install, is asked
/// again. A guest that publishes is asked every time, since that costs no
/// failed call.
fn probe_guest(libvirt: &crate::attest::Libvirt, vm: &str) -> (Published, String) {
    let id = libvirt
        .virsh(&["domid", vm])
        .map(|id| id.trim().to_owned())
        .unwrap_or_default();
    let now = std::time::Instant::now();
    let mut probes = PROBES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let probes = probes.get_or_insert_with(Probes::new);
    if let Some((kept_id, at, published, detail)) = probes.get(vm) {
        if *kept_id == id && !id.is_empty() && now.saturating_duration_since(*at) < PROBE_KEPT {
            return (published.clone(), detail.clone());
        }
    }
    let (published, _, detail) = read_guest_publication(libvirt, vm);
    if matches!(published, Published::Yes { .. }) {
        probes.remove(vm);
    } else {
        probes.insert(vm.to_owned(), (id, now, published.clone(), detail.clone()));
    }
    (published, detail)
}

/// Forget the kept guest probes, so the next list asks every guest: after
/// a change, or when a person reads again.
pub fn forget_guest_probes() {
    if let Ok(mut probes) = PROBES.lock() {
        *probes = None;
    }
}

/// What each VM's disk says about the system in it, once read: reading a
/// disk takes about ten seconds, so each is read once, in the
/// background, and kept until [`forget_disks`].
enum DiskRead {
    Reading,
    Read(Result<crate::guest::DiskSystem, String>),
}

static DISKS: std::sync::Mutex<Option<std::collections::HashMap<String, DiskRead>>> =
    std::sync::Mutex::new(None);

/// Read the disks of `vms` not read yet, one after another on a thread of
/// their own, calling `read` after each. Nothing is read where libguestfs
/// isn't installed.
pub fn read_disks(vms: Vec<String>, read: impl Fn() + Send + 'static) {
    static AVAILABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*AVAILABLE.get_or_init(crate::guest::offline_available) {
        return;
    }
    let todo: Vec<String> = {
        let mut disks = DISKS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let disks = disks.get_or_insert_with(Default::default);
        let todo: Vec<String> = vms
            .into_iter()
            .filter(|vm| !disks.contains_key(vm))
            .collect();
        for vm in &todo {
            disks.insert(vm.clone(), DiskRead::Reading);
        }
        todo
    };
    if todo.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for vm in todo {
            let system = crate::guest::inspect_disk(&vm);
            if let Ok(mut disks) = DISKS.lock() {
                disks
                    .get_or_insert_with(Default::default)
                    .insert(vm, DiskRead::Read(system));
            }
            read();
        }
    });
}

/// Forget the disks read, so they are read again.
pub fn forget_disks() {
    if let Ok(mut disks) = DISKS.lock() {
        disks.take();
    }
}

/// What the disk read says would stop `vm add` for `vm`: a system without
/// dnf or apt, as `vm add` refuses.
fn disk_problem(vm: &str) -> Option<Problem> {
    let disks = DISKS.lock().ok()?;
    match disks.as_ref()?.get(vm)? {
        DiskRead::Read(Ok(system)) => no_packages(system),
        _ => None,
    }
}

/// The problem with a system rampire-guest can't be installed in.
fn no_packages(system: &crate::guest::DiskSystem) -> Option<Problem> {
    (!system.takes_guest()).then(|| Problem {
        problem: format!(
            "it runs {}, whose system is read only, so rampire-guest can't be installed in it",
            system
                .product
                .as_deref()
                .unwrap_or("a system RAMpire doesn't recognise")
        ),
        fix: "none: the guest service needs a writable /usr/local and /etc, or dnf or apt"
            .to_owned(),
    })
}

/// The guest service's unit, as the guest package installs it; a local
/// install's is [`crate::guest::LOCAL_UNIT`].
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
                suggested: None,
                rampire_parts: false,
                add_restarts: false,
                runs_behind: false,
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
        suggested: None,
        rampire_parts: false,
        add_restarts: false,
        runs_behind: false,
    };
    match management {
        Management::Managed if state == "running" => {
            vm.runs_behind = runs_behind(libvirt, name);
            managed_status(context, &mut vm, &state);
        }
        Management::Managed => managed_status(context, &mut vm, &state),
        Management::Unfinished => {
            vm.detail =
                format!("an earlier vm add didn't finish: rampire vm add {name} finishes it");
        }
        Management::NotManaged => {
            match libvirt
                .virsh(&["dumpxml", "--inactive", name])
                .and_then(|xml| preview(&xml))
            {
                Ok(preview) => {
                    vm.to_fix = preview.to_fix;
                    vm.problems = preview.problems;
                    vm.suggested = preview.suggested;
                    vm.rampire_parts = preview.parts;
                    vm.add_restarts =
                        state == "running" && (preview.restarts || runs_behind(libvirt, name));
                }
                Err(error) => vm.problems.push(Problem {
                    problem: format!("its definition couldn't be read: {error}"),
                    fix: "check the domain with virsh dumpxml".to_owned(),
                }),
            }
            vm.problems.extend(disk_problem(name));
            if state == "running" {
                let (published, detail) = probe_guest(libvirt, name);
                vm.guest = published;
                vm.detail = detail;
            } else {
                vm.detail = format!("{state}: start it to see whether its guest service runs");
            }
        }
    }
    vm
}

/// Whether the running VM `name` runs an older definition than its saved
/// one, as `doctor` and `vm add` judge it. Unknown is no.
fn runs_behind(libvirt: &crate::attest::Libvirt, name: &str) -> bool {
    let read = |args: &[&str]| {
        libvirt
            .virsh(args)
            .ok()
            .and_then(|xml| crate::vm::inspect(&xml).ok())
    };
    match (
        read(&["dumpxml", "--inactive", name]),
        read(&["dumpxml", name]),
    ) {
        (Some(saved), Some(live)) => saved.runs_behind(&live),
        _ => false,
    }
}

/// A managed VM's status and its line, into `vm`.
fn managed_status(context: &Context, vm: &mut ListedVm, state: &str) {
    let name = vm.vm_name.clone();
    match read_status(context, &name) {
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
    }
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
        let note = |out: &mut String, text: String| {
            out.push_str(&format!("{:width$}  {text}\n", ""));
        };
        if vm.management == Management::Unfinished {
            note(
                &mut out,
                format!(
                    "an add stopped part-way: rampire vm add {0} finishes it, rampire vm remove {0} undoes it",
                    vm.vm_name
                ),
            );
        }
        if vm.management == Management::NotManaged {
            // What `vm add` does, not problems for a person to fix.
            for change in &vm.to_fix {
                note(&mut out, format!("adding it will {change}"));
            }
            if vm.rampire_parts {
                note(
                    &mut out,
                    format!(
                        "its definition still has RAMpire's parts from an earlier add: rampire vm remove {} takes them out",
                        vm.vm_name
                    ),
                );
            }
            for problem in &vm.problems {
                out.push_str(&format!(
                    "{:width$}  problem: {}\n{:width$}    fix: {}\n",
                    "", problem.problem, "", problem.fix
                ));
            }
            if vm.problems.is_empty() {
                out.push_str(&format!(
                    "{:width$}  add it with: rampire vm add {}\n",
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
        let (to_fix, problems, parts) = add_preview(PLAIN).unwrap();
        assert!(!parts, "a plain definition has none of RAMpire's parts");
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
    fn a_read_only_system_is_a_problem_before_add() {
        let system = |product: &str, root: &str| crate::guest::DiskSystem {
            product: Some(product.to_owned()),
            package_format: None,
            root_filesystem: Some(root.to_owned()),
        };
        let problem =
            no_packages(&system("Home Assistant OS 18.4", "squashfs")).expect("a problem");
        assert!(problem.problem.contains("Home Assistant OS"), "{problem:?}");
        // No dnf or apt on a writable root takes the static binary.
        assert_eq!(no_packages(&system("Arch Linux", "ext4")), None);
        assert_eq!(disk_problem("a-vm-whose-disk-was-never-read"), None);
    }

    #[test]
    fn vms_are_managed_unfinished_or_not_managed() {
        let known = Known {
            managed: ["web".to_owned()].into(),
            unfinished: ["half".to_owned()].into(),
        };
        assert_eq!(known.management("web"), Management::Managed);
        assert_eq!(known.management("half"), Management::Unfinished);
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
                suggested: None,
                rampire_parts: false,
                add_restarts: false,
                runs_behind: false,
            }],
        };
        assert_eq!(
            text(&report),
            "web  not managed  running   published 3s ago\n     adding it will add a 6144 MiB virtio-mem device\n     add it with: rampire vm add web\n"
        );
        // Left over from an earlier add, and an add that stopped part-way.
        let mut leftover = report.clone();
        leftover.vms[0].rampire_parts = true;
        leftover.vms[0].to_fix.clear();
        assert!(
            text(&leftover).contains("rampire vm remove web takes them out"),
            "{}",
            text(&leftover)
        );
        let mut unfinished = report.clone();
        unfinished.vms[0].management = Management::Unfinished;
        assert!(
            text(&unfinished).contains("rampire vm add web finishes it"),
            "{}",
            text(&unfinished)
        );
    }
}
