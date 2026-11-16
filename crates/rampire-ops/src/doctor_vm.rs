//! `rampire doctor VM`: everything that can be checked about one VM, each
//! problem with its fix, as `rampire doctor` checks the host. It reads; it
//! changes nothing.
//!
//! It checks the VM's saved and running definition (what `vm add`
//! inspects), the running VM against its recorded setup (`check`), a latch, the
//! guest's publication and its readiness report, and the last request that
//! was refused or held back.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use rampire_core::{ControlHealth, ControllerStatusSnapshot, RawTelemetryEnvelope};
use rampire_host::config::{HostConfig, HostSettings};

use crate::context::Context;
use crate::doctor::Finding;
use crate::inventory::Known;
use crate::reports::{CheckedVm, Management};

/// Every check for `vm`.
pub fn run(context: &Context, vm: &str) -> Result<Vec<Finding>, String> {
    let known = Known::read(context)?;
    let libvirt = context.libvirt()?;
    let management = known.management(vm);
    if management != Management::Managed {
        return Ok(unmanaged(&libvirt, vm, management));
    }
    let settings = HostSettings::load(&context.running()).map_err(|error| error.to_string())?;
    let config = settings
        .vms
        .into_iter()
        .find(|config| config.vm_name == vm)
        .ok_or_else(|| format!("{vm} is not in the configuration the controller runs"))?;

    let mut findings = Vec::new();
    let running = libvirt.state(vm).is_ok_and(|state| state == "running");
    match libvirt.virsh(&["dumpxml", "--inactive", vm]) {
        Ok(saved) => {
            let live = if running {
                libvirt.virsh(&["dumpxml", vm]).ok()
            } else {
                None
            };
            findings.extend(definition(vm, &config.alias, &saved, live.as_deref())?);
        }
        Err(error) => findings.push(
            Finding::problem(
                "definition",
                format!("libvirt couldn't give {vm}'s definition: {error}"),
                format!("if {vm} was deleted, stop managing it with: rampire vm remove {vm}"),
            )
            .runs(&["vm", "remove", vm]),
        ),
    }

    findings.push(attestation(context, vm)?);

    if !running {
        findings.push(Finding::problem(
            "running",
            format!(
                "{vm} is {}",
                libvirt.state(vm).unwrap_or_else(|_| "unknown".to_owned())
            ),
            format!("start it: virsh start {vm}; the controller resizes it while it runs"),
        ));
        return Ok(findings);
    }

    match crate::inventory::read_status(context, vm) {
        Ok(status) => findings.extend(controller(vm, &status, crate::vm::boot_bytes(&libvirt, vm))),
        Err(error) => findings.push(Finding::problem(
            "controller status",
            format!("{vm}'s status couldn't be read: {error}"),
            "check the controller: rampire status, and rampire logs",
        )),
    }

    let publication = read_publication(vm);
    findings.push(publication_age(
        publication.as_ref(),
        crate::process::unix_millis()?,
    ));
    if let Some(publication) = &publication {
        findings.extend(guest_build(vm, context.build_id(), publication));
        findings.extend(readiness(publication, config.max_ram));
    }

    findings.push(last_request(&config, Path::new("/")));
    Ok(findings)
}

/// Whether the guest service runs the build this host carries for it.
/// Nothing when either build is unknown.
pub fn guest_build(
    vm: &str,
    host: Option<&str>,
    publication: &RawTelemetryEnvelope,
) -> Option<Finding> {
    let check = "guest build";
    let guest = crate::guest::publication_build(Some(publication))?;
    let host = host?;
    Some(if guest == host {
        Finding::ok(check, format!("{guest}, the build this host runs"))
    } else {
        Finding::problem(
            check,
            format!("an update is available: the guest runs build {guest}, this host {host}"),
            format!("rampire vm upgrade {vm}"),
        )
        .runs(&["vm", "upgrade", vm])
    })
}

/// The controller's copy of the guest's publication, if it has one.
fn read_publication(vm: &str) -> Option<RawTelemetryEnvelope> {
    let path = rampire_host::guest_channel::publication_copy_path(vm);
    let bytes = rampire_core::files::read_bounded(&path, crate::host_readings::MAX_TELEMETRY_BYTES)
        .ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// A VM RAMpire doesn't manage: how to bring it under management.
fn unmanaged(libvirt: &crate::attest::Libvirt, vm: &str, management: Management) -> Vec<Finding> {
    match management {
        Management::Unfinished => vec![Finding::problem(
            "managed",
            format!("an earlier vm add {vm} didn't finish"),
            format!("rampire vm add {vm}"),
        )
        .runs(&["vm", "add", vm])],
        _ => {
            // A system the guest service can't go in, read from its disk as
            // `vm add` reads it: adding it would only be refused.
            let read_only = crate::guest::offline_available()
                .then(|| crate::guest::inspect_disk(vm).ok())
                .flatten()
                .filter(|system| !system.takes_guest());
            let mut findings = vec![match &read_only {
                Some(system) => Finding::problem(
                    "managed",
                    format!(
                        "RAMpire doesn't manage {vm}, and can't: it runs {}, whose system is read only, so the guest service can't be installed",
                        system.product.as_deref().unwrap_or("a system RAMpire doesn't recognise")
                    ),
                    "none: RAMpire manages VMs whose system can take its guest service",
                ),
                None => Finding::problem(
                    "managed",
                    format!("RAMpire doesn't manage {vm}"),
                    format!("rampire vm add {vm}"),
                )
                .runs(&["vm", "add", vm]),
            }];
            match libvirt
                .virsh(&["dumpxml", "--inactive", vm])
                .and_then(|xml| crate::inventory::add_preview(&xml))
            {
                Ok((to_fix, problems, parts)) => {
                    if parts {
                        findings.push(
                            Finding::problem(
                                "leftover parts",
                                "its definition still has RAMpire's device or control channel from an earlier add",
                                format!("rampire vm remove {vm} takes them out"),
                            )
                            .runs(&["vm", "remove", vm]),
                        );
                    }
                    // What an add would do, for a person to know: not problems.
                    if read_only.is_none() {
                        for change in to_fix {
                            findings.push(Finding::ok("vm add would", change));
                        }
                    }
                    for problem in problems {
                        findings.push(Finding::problem("definition", problem.problem, problem.fix));
                    }
                }
                Err(error) => findings.push(Finding::problem(
                    "definition",
                    format!("libvirt couldn't give {vm}'s definition: {error}"),
                    "check the name: rampire vm list",
                )),
            }
            findings
        }
    }
}

/// The saved definition, as `vm add` inspects it, and the running one.
pub fn definition(
    vm: &str,
    alias: &str,
    saved: &str,
    live: Option<&str>,
) -> Result<Vec<Finding>, String> {
    let inspection = crate::vm::inspect(saved)?;
    let mut findings = Vec::new();
    let redo = format!("rampire vm remove {vm}, then rampire vm add {vm}");
    for (problem, fix) in inspection.problems() {
        findings.push(Finding::problem("definition", problem, fix));
    }
    if let Some(device) = inspection.device() {
        match device.alias.as_deref() {
            Some(found) if found == alias => findings.push(Finding::ok(
                "virtio-mem device",
                format!(
                    "{alias}, {} MiB in {} MiB blocks",
                    device.size_bytes >> 20,
                    device.block_bytes >> 20
                ),
            )),
            found => findings.push(
                Finding::problem(
                    "virtio-mem device",
                    format!(
                        "the settings name the device {alias}, but the definition's is {}",
                        found.unwrap_or("unnamed")
                    ),
                    format!(
                        "rampire set {vm} alias={}",
                        found.unwrap_or("ua-virtiomem0")
                    ),
                )
                .runs(&[
                    "set",
                    vm,
                    &format!("alias={}", found.unwrap_or("ua-virtiomem0")),
                ]),
            ),
        }
    }
    findings.push(if inspection.control_channel {
        Finding::ok(
            "control channel",
            format!("{} is defined", rampire_core::MEMORY_CONTROL_CHANNEL_NAME),
        )
    } else {
        Finding::problem(
            "control channel",
            format!(
                "the definition has no {} channel, so the guest can't reach the controller",
                rampire_core::MEMORY_CONTROL_CHANNEL_NAME
            ),
            redo.clone(),
        )
    });
    findings.push(match inspection.balloon.as_deref() {
        Some(model) if model != "none" => Finding::problem(
            "memory balloon",
            format!("a {model} balloon could resize the VM against RAMpire"),
            redo.clone(),
        ),
        _ => Finding::ok("memory balloon", "off"),
    });
    if let Some(slots) = inspection.max_memory_slots {
        let used = inspection.other_memory_devices + inspection.devices.len();
        findings.push(if slots as usize >= used {
            Finding::ok(
                "memory slots",
                format!(
                    "{slots} in <maxMemory>, {} free after its memory devices",
                    slots as usize - used
                ),
            )
        } else {
            Finding::problem(
                "memory slots",
                format!("<maxMemory> has {slots} slots for {used} memory devices"),
                format!("raise <maxMemory slots> to {used} or more with virsh edit {vm}"),
            )
        });
    }
    if let Some(live) = live {
        let running = crate::vm::inspect(live)?;
        findings.push(if inspection.runs_behind(&running) {
            Finding::problem(
                "running definition",
                format!("{vm} isn't running its saved definition"),
                format!("rampire vm restart {vm}"),
            )
            .runs(&["vm", "restart", vm])
        } else {
            Finding::ok(
                "running definition",
                "the running VM has the saved device and channel",
            )
        });
    }
    Ok(findings)
}

/// The running VM against the setup RAMpire recorded, as `check` reads it.
fn attestation(context: &Context, vm: &str) -> Result<Finding, String> {
    let report = crate::check::run(&context.running())?;
    Ok(
        match report.vms.into_iter().find(|checked| checked.vm_name == vm) {
            Some(CheckedVm { problem: None, .. }) => {
                Finding::ok("recorded setup", "matches the running VM")
            }
            Some(CheckedVm {
                problem: Some(problem),
                ..
            }) => {
                let fix = crate::check::fix(vm, &problem);
                let words = crate::check::fix_words(vm, &problem);
                let words: Vec<&str> = words.iter().map(String::as_str).collect();
                Finding::problem("recorded setup", problem, fix).runs(&words)
            }
            None => Finding::problem(
                "recorded setup",
                format!("{vm} isn't in the configuration the controller runs"),
                "rampire apply",
            )
            .runs(&["apply"]),
        },
    )
}

/// What the controller says: a latch, recovery, observe-only. `boot` is what
/// the VM starts with, for its sizes ([`crate::reports::status_line`]).
pub fn controller(vm: &str, status: &ControllerStatusSnapshot, boot: Option<u64>) -> Vec<Finding> {
    let recover = format!("rampire recover {vm} \"WHY IT IS SAFE TO RESUME\"");
    let mut findings = Vec::new();
    if status.actuation_latched {
        findings.push(
            Finding::problem(
                "latch",
                format!(
                    "resizing stopped: {}",
                    status
                        .latch_reason
                        .as_deref()
                        .unwrap_or("no reason recorded")
                ),
                format!("once the guest is checked, {recover}"),
            )
            .runs(&["recover", vm]),
        );
    } else if status.control_health == ControlHealth::RecoveryRequired {
        findings.push(
            Finding::problem(
                "latch",
                "the controller needs a person to recover this VM",
                format!("once the guest is checked, {recover}"),
            )
            .runs(&["recover", vm]),
        );
    } else {
        findings.push(Finding::ok("latch", "none"));
    }
    findings.push(Finding::ok(
        "controller",
        // The line says "(observe only)"; this says what that means.
        crate::reports::status_line(status, boot).replace(
            " (observe only)",
            " (observe only: it decides but changes nothing)",
        ),
    ));
    findings
}

/// Whether the guest has published, and how long ago.
pub fn publication_age(publication: Option<&RawTelemetryEnvelope>, now_millis: u64) -> Finding {
    let check = "guest publication";
    let install = "check the guest service in the VM: systemctl status rampire-guest";
    match publication {
        None => Finding::problem(
            check,
            "the controller has no publication from the guest",
            install,
        ),
        Some(publication) => {
            let age =
                Duration::from_millis(now_millis.saturating_sub(publication.observed_unix_millis));
            let build = publication
                .diagnostics
                .as_ref()
                .map(|diagnostics| format!(", from build {}", diagnostics.build_id))
                .unwrap_or_default();
            if age >= crate::top::STALE_PUBLICATION {
                Finding::problem(
                    check,
                    format!("the newest publication is {}s old{build}", age.as_secs()),
                    install,
                )
            } else {
                Finding::ok(check, format!("{}s old{build}", age.as_secs()))
            }
        }
    }
}

/// The guest's readiness report: its driver, how hot-plugged memory is
/// onlined, its memory-pressure signal and whether `max_ram` fits what
/// the auto-movable policy keeps removable (docs/administration.md).
pub fn readiness(publication: &RawTelemetryEnvelope, max_ram: u64) -> Vec<Finding> {
    let Some(environment) = publication
        .diagnostics
        .as_ref()
        .map(|diagnostics| &diagnostics.environment)
        .filter(|environment| !environment.is_empty())
    else {
        return vec![Finding::problem(
            "guest readiness",
            "the guest sends no readiness report",
            "upgrade rampire-guest in the VM to the host's build",
        )];
    };
    let fact = |name: &str| {
        environment
            .get(name)
            .map(String::as_str)
            .unwrap_or("unknown")
    };
    let mut findings = Vec::new();
    findings.push(if fact("virtio_mem_driver") == "bound" {
        Finding::ok("virtio-mem driver", "bound")
    } else {
        Finding::problem(
            "virtio-mem driver",
            "no device is bound to the virtio-mem driver: this guest can't be resized",
            "load it in the guest (modprobe virtio_mem); its kernel needs CONFIG_VIRTIO_MEM",
        )
    });
    findings.push(if fact("pressure_signal") == "none" {
        Finding::problem(
            "pressure signal",
            "pressure stall information is off, so the guest asks for memory but never returns any",
            "boot the guest with psi=1 (on RHEL: grubby --update-kernel=ALL --args=psi=1), then restart the VM",
        )
    } else {
        Finding::ok("pressure signal", fact("pressure_signal"))
    });
    findings.push(onlining(environment));
    if let Some(finding) = movable_limit(environment, max_ram) {
        findings.push(finding);
    }
    findings
}

fn onlining(environment: &BTreeMap<String, String>) -> Finding {
    let fact = |name: &str| {
        environment
            .get(name)
            .map(String::as_str)
            .unwrap_or("unknown")
    };
    let check = "onlining";
    let auto_online = fact("auto_online_blocks");
    let policy = fact("online_policy");
    let rule = fact("udev_memory_rule");
    let movable = fact("movable_zone_pages")
        .parse::<u64>()
        .map(|pages| format!(", {} MiB in the movable zone", (pages * 4096) >> 20))
        .unwrap_or_default();
    match auto_online {
        "offline" if rule != "none" && rule != "unknown" => {
            Finding::ok(check, format!("by the udev rule {rule}{movable}"))
        }
        "offline" => Finding::problem(
            check,
            "auto_online_blocks is offline: hot-plugged memory stays unused",
            "boot the guest with memhp_default_state=online memory_hotplug.online_policy=auto-movable",
        ),
        // As RHEL's udev rule does, and as measured it gives memory back:
        // fine, with what to do should removals come back partial
        // (docs/administration.md, Linux guests).
        "online" if policy != "auto-movable" => Finding::ok(
            check,
            format!(
                "online, online_policy={policy}{movable}; if removals come back partial, boot the guest with memory_hotplug.online_policy=auto-movable"
            ),
        ),
        "online_kernel" => Finding::problem(
            check,
            "hot-plugged memory is onlined to kernel zones (online_kernel): removal is guaranteed only from the movable zone",
            "boot the guest with memhp_default_state=online memory_hotplug.online_policy=auto-movable",
        ),
        "unknown" => Finding::problem(
            check,
            "the guest didn't say how it onlines hot-plugged memory",
            "upgrade rampire-guest in the VM to the host's build",
        ),
        mode => Finding::ok(check, format!("{mode}, online_policy={policy}{movable}")),
    }
}

/// Whether `max_ram` is more than auto-movable keeps removable: hot-plugged
/// memory goes to the movable zone only up to `auto_movable_ratio` percent
/// of the boot memory. CMA, which the guest's own check subtracts, isn't
/// published, so this is the limit without it.
fn movable_limit(environment: &BTreeMap<String, String>, max_ram: u64) -> Option<Finding> {
    let fact = |name: &str| environment.get(name).map(String::as_str);
    if fact("online_policy") != Some("auto-movable") || fact("auto_online_blocks") != Some("online")
    {
        return None;
    }
    let boot: u64 = fact("boot_ram_bytes")?.parse().ok()?;
    let ratio: u64 = fact("auto_movable_ratio")?.parse().ok()?;
    let reachable = boot.saturating_add(
        u64::try_from(u128::from(boot) * u128::from(ratio) / 100).unwrap_or(u64::MAX),
    );
    Some(if max_ram <= reachable {
        Finding::ok(
            "movable zone",
            format!(
                "max_ram {} MiB is within the {} MiB auto-movable keeps removable",
                max_ram >> 20,
                reachable >> 20
            ),
        )
    } else {
        Finding::problem(
            "movable zone",
            format!(
                "max_ram {} MiB is more than the {} MiB auto-movable keeps removable (boot RAM {} MiB, auto_movable_ratio={ratio}): memory past it may not come back",
                max_ram >> 20,
                reachable >> 20,
                boot >> 20
            ),
            "raise boot memory, lower max_ram (rampire set VM max_ram=SIZE) or raise memory_hotplug.auto_movable_ratio in the guest",
        )
    })
}

/// The last request the controller finished for this VM, and what held it
/// back if it was refused or partial.
fn last_request(config: &HostConfig, cwd: &Path) -> Finding {
    let check = "last request";
    let journal = match crate::journal::read_events(crate::systemd_unit::CONTROLLER_UNIT, cwd) {
        Ok(journal) => journal,
        Err(reason) => return Finding::problem(check, reason, "run it with sudo"),
    };
    let lines = crate::watch::lines_for(&journal, &config.vm_name);
    last_request_finding(&config.vm_name, &lines)
}

pub(crate) fn last_request_finding(vm: &str, lines: &[&str]) -> Finding {
    let check = "last request";
    let Some(result) = crate::watch::parse_last_result(lines) else {
        return Finding::ok(check, "none in the journal yet");
    };
    if result.journal_outcome == "Completed" {
        return Finding::ok(check, format!("completed: {}", result.detail));
    }
    let fixes: Vec<String> = result
        .limiting_factors
        .iter()
        .filter_map(|factor| match factor.as_str() {
            "host_headroom" => Some(
                "the host is near its reserve: free host memory, or lower host_reserve (rampire set global host_reserve=SIZE)".to_owned(),
            ),
            "pool" => Some(
                "the pool is fully used: raise the pool (rampire set global pool=SIZE) or this VM's priority".to_owned(),
            ),
            "guest_maximum" => Some(format!("raise max_ram: rampire set {vm} max_ram=SIZE")),
            "device_capacity" => Some(format!(
                "the virtio-mem device is full: give {vm} a larger device with virsh edit, then rampire vm restart {vm}"
            )),
            _ => None,
        })
        .collect();
    let detail = format!("{}: {}", result.outcome, result.detail);
    match fixes.is_empty() {
        // A removal the guest couldn't complete, or an outcome with no limit
        // named: the journal says why.
        true => Finding::problem(check, detail, format!("see rampire logs --vm {vm}")),
        false => Finding::problem(check, detail, fixes.join("; ")),
    }
}

/// One line for each managed VM, for `rampire doctor` without a VM.
pub fn summaries(context: &Context) -> Vec<Finding> {
    let Ok(known) = Known::read(context) else {
        return Vec::new();
    };
    known
        .managed
        .iter()
        .map(|vm| summary(vm, run(context, vm)))
        .collect()
}

pub fn summary(vm: &str, findings: Result<Vec<Finding>, String>) -> Finding {
    let check = format!("vm {vm}");
    match findings {
        Err(error) => {
            Finding::problem(&check, error, format!("rampire doctor {vm}")).runs(&["doctor", vm])
        }
        Ok(findings) => {
            let problems: Vec<&Finding> = findings.iter().filter(|finding| !finding.ok).collect();
            match problems.as_slice() {
                [] => Finding::ok(&check, format!("{} checks passed", findings.len())),
                [only] => Finding::problem(
                    &check,
                    format!("{}: {}", only.check, only.detail),
                    format!("rampire doctor {vm}"),
                )
                .runs(&["doctor", vm]),
                [first, rest @ ..] => Finding::problem(
                    &check,
                    format!("{}: {}, and {} more", first.check, first.detail, rest.len()),
                    format!("rampire doctor {vm}"),
                )
                .runs(&["doctor", vm]),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANAGED: &str = "<domain type='kvm'><name>web</name><maxMemory slots='2' unit='KiB'>8388608</maxMemory><memory unit='KiB'>2097152</memory><vcpu>2</vcpu><cpu><numa><cell id='0' cpus='0-1' memory='2097152' unit='KiB'/></numa></cpu><devices><channel type='unix'><target type='virtio' name='org.rampire.control.0'/></channel><memory model='virtio-mem'><target><size unit='KiB'>6291456</size><block unit='KiB'>2048</block><requested unit='KiB'>0</requested></target><alias name='ua-virtiomem0'/></memory><memballoon model='none'/></devices></domain>";

    fn problems(findings: &[Finding]) -> Vec<&str> {
        findings
            .iter()
            .filter(|finding| !finding.ok)
            .map(|finding| finding.check.as_str())
            .collect()
    }

    #[test]
    fn a_well_set_up_definition_passes_every_check() {
        let findings = definition("web", "ua-virtiomem0", MANAGED, Some(MANAGED)).unwrap();
        assert_eq!(problems(&findings), Vec::<&str>::new(), "{findings:?}");
        assert!(findings
            .iter()
            .any(|finding| finding.detail.contains("1 free")));
    }

    #[test]
    fn a_definition_problem_says_how_to_fix_it() {
        let ballooned = MANAGED.replace("model='none'", "model='virtio'");
        let unchanneled = ballooned.replace("org.rampire.control.0", "org.other");
        let findings = definition("web", "ua-virtiomem0", &unchanneled, Some(MANAGED)).unwrap();
        assert_eq!(
            problems(&findings),
            ["control channel", "memory balloon", "running definition"]
        );
        let balloon = findings
            .iter()
            .find(|finding| finding.check == "memory balloon")
            .unwrap();
        assert_eq!(
            balloon.fix.as_deref(),
            Some("rampire vm remove web, then rampire vm add web")
        );
        let renamed = definition("web", "ua-other", MANAGED, None).unwrap();
        assert_eq!(problems(&renamed), ["virtio-mem device"]);
    }

    fn publication(environment: &[(&str, &str)]) -> RawTelemetryEnvelope {
        let text = include_str!("../../../fixtures/publication.json");
        let mut publication: RawTelemetryEnvelope = serde_json::from_str(text).unwrap();
        publication.diagnostics.as_mut().unwrap().environment = environment
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        publication
    }

    const READY: [(&str, &str); 6] = [
        ("virtio_mem_driver", "bound"),
        ("pressure_signal", "psi"),
        ("auto_online_blocks", "online"),
        ("online_policy", "auto-movable"),
        ("boot_ram_bytes", "2147483648"),
        ("auto_movable_ratio", "301"),
    ];

    #[test]
    fn a_ready_guest_passes_and_one_that_isnt_says_what_to_change() {
        let ready = readiness(&publication(&READY), 8 << 30);
        assert_eq!(problems(&ready), Vec::<&str>::new(), "{ready:?}");

        // 2 GiB boot at ratio 301 keeps 2 + 6.02 GiB removable.
        let too_big = readiness(&publication(&READY), 9 << 30);
        assert_eq!(problems(&too_big), ["movable zone"]);

        let mut unready = READY.to_vec();
        unready[1] = ("pressure_signal", "none");
        unready[2] = ("auto_online_blocks", "online_kernel");
        let unready = readiness(&publication(&unready), 4 << 30);
        assert_eq!(problems(&unready), ["pressure signal", "onlining"]);
        assert!(unready[1].fix.as_deref().unwrap().contains("psi=1"));

        // Debian's default onlining gives memory back, as measured.
        let mut debian = READY.to_vec();
        debian[3] = ("online_policy", "contig-zones");
        let debian = readiness(&publication(&debian), 4 << 30);
        assert_eq!(problems(&debian), Vec::<&str>::new(), "{debian:?}");

        assert_eq!(
            problems(&readiness(&publication(&[]), 1)),
            ["guest readiness"]
        );
    }

    #[test]
    fn a_guest_of_another_build_has_an_update() {
        let guest = publication(&READY);
        let build = crate::guest::publication_build(Some(&guest))
            .unwrap()
            .to_owned();
        assert!(guest_build("web", Some(&build), &guest).unwrap().ok);
        let behind = guest_build("web", Some("newer"), &guest).unwrap();
        assert!(!behind.ok);
        assert_eq!(behind.fix.as_deref(), Some("rampire vm upgrade web"));
        assert!(guest_build("web", None, &guest).is_none());
    }

    #[test]
    fn a_stale_or_missing_publication_is_a_problem() {
        let fresh = publication(&READY);
        let at = fresh.observed_unix_millis;
        assert!(publication_age(Some(&fresh), at + 2_000).ok);
        assert!(!publication_age(Some(&fresh), at + 31_000).ok);
        assert!(!publication_age(None, at).ok);
    }

    #[test]
    fn a_held_back_request_says_what_to_change() {
        let journal = "\
2026-11-14T16:12:59+00:00 h rampire[1]: rampire: vm=web event=grant_limits session_id=s sequence=4 operation=Add bytes=127926272 limiting_factors=guest_maximum,pool alignment_remainder_bytes=0
2026-11-14T16:12:59+00:00 h rampire[1]: rampire: vm=web event=request_completed operation_id=none session_id=s sequence=4 operation=Add bytes=127926272 outcome=Partial actual_bytes=0
";
        let lines: Vec<&str> = journal.lines().collect();
        let finding = last_request_finding("web", &lines);
        assert!(!finding.ok);
        let fix = finding.fix.unwrap();
        assert!(fix.contains("rampire set web max_ram=SIZE"), "{fix}");
        assert!(fix.contains("rampire set global pool=SIZE"), "{fix}");
        assert!(last_request_finding("web", &[]).ok);
    }

    #[test]
    fn a_summary_names_the_first_problem_and_how_many_more() {
        let findings = vec![
            Finding::ok("latch", "none"),
            Finding::problem("pressure signal", "off", "psi=1"),
            Finding::problem("onlining", "kernel zones", "auto-movable"),
        ];
        let line = summary("web", Ok(findings));
        assert_eq!(line.check, "vm web");
        assert_eq!(line.detail, "pressure signal: off, and 1 more");
        assert_eq!(line.fix.as_deref(), Some("rampire doctor web"));
        assert!(summary("web", Ok(vec![Finding::ok("latch", "none")])).ok);
    }
}
