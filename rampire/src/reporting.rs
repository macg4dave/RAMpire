//! The read-only reports: `status`, `check` and `doctor`, as text for a
//! person or as a versioned JSON document with `--json`.

use std::process::ExitCode;

use rampire_core::{ControlHealth, ControllerStatusSnapshot};
use rampire_host::config::HostSettings;
use rampire_ops::reports::{
    CheckReport, CheckSchema, CheckedVm, DoctorReport, DoctorSchema, StatusReport,
    StatusReportSchema,
};
use serde::Serialize;

use crate::{
    applied_root, connection, draft, exit, libvirt, running, target, Cli, SERVICE_ACCOUNT,
};

pub(crate) fn status(cli: &Cli, vm: Option<&str>) -> Result<ExitCode, String> {
    let names = match vm {
        Some(vm) => vec![vm.to_owned()],
        None => HostSettings::load(&running(cli))
            .map_err(|error| error.to_string())?
            .vms
            .into_iter()
            .map(|config| config.vm_name)
            .collect(),
    };
    // A VM that can't be read, usually because it isn't running, is
    // reported on its own; the others are still shown.
    let mut vms = Vec::new();
    let mut unavailable = Vec::new();
    for name in &names {
        let mut bytes = Vec::new();
        let read = rampire_host::operator_actions::status(&target(cli, Some(name))?, &mut bytes)
            .and_then(|()| {
                rampire_core::parse_controller_status(&bytes).map_err(|error| error.to_string())
            });
        match read {
            Ok(status) => vms.push(status),
            Err(reason) => unavailable.push(rampire_ops::reports::UnavailableVm {
                vm_name: name.clone(),
                state: libvirt(cli)
                    .and_then(|libvirt| libvirt.state(name))
                    .unwrap_or_else(|_| "unknown".to_owned()),
                reason,
            }),
        }
    }
    let controller = rampire_ops::systemd_unit::controller_state();
    if cli.json {
        print_json(&StatusReport {
            schema: StatusReportSchema,
            version: 1,
            controller,
            vms,
            unavailable,
        })?;
        return Ok(ExitCode::SUCCESS);
    }
    println!(
        "{}: {} ({}), {}",
        controller.unit, controller.active_state, controller.sub_state, controller.unit_file_state
    );
    for vm in &unavailable {
        println!(
            "{}: {}, so its status can't be read ({})",
            vm.vm_name, vm.state, vm.reason
        );
    }
    if vms.is_empty() && unavailable.is_empty() {
        println!("RAMpire manages no VMs yet. Add one with: rampire vm add VM --fix-xml");
    }
    for status in &vms {
        println!("{}", status_line(status));
    }
    Ok(ExitCode::SUCCESS)
}

/// One VM's status in a line a person can read.
pub(crate) fn status_line(status: &ControllerStatusSnapshot) -> String {
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

fn size(bytes: u64) -> String {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

fn print_json<T: Serialize>(document: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(document).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

pub(crate) fn check(cli: &Cli) -> Result<ExitCode, String> {
    let report = rampire_ops::check::run(&draft(cli))?;
    let code = ExitCode::from(if report.passed() {
        exit::SUCCESS
    } else {
        exit::FAILURE
    });
    if cli.json {
        print_json(&CheckReport {
            schema: CheckSchema,
            version: 1,
            passed: report.passed(),
            vms: report
                .vms
                .into_iter()
                .map(|vm| CheckedVm {
                    vm_name: vm.vm_name,
                    problem: vm.problem,
                })
                .collect(),
        })?;
        return Ok(code);
    }
    if report.vms.is_empty() {
        println!("ok: the configuration is valid and manages no VMs");
    }
    for vm in &report.vms {
        match &vm.problem {
            None => println!("ok: {}", vm.vm_name),
            Some(problem) => println!("problem: {}: {problem}", vm.vm_name),
        }
    }
    Ok(code)
}

pub(crate) fn doctor(cli: &Cli) -> Result<ExitCode, String> {
    let findings = rampire_ops::doctor::run(&connection(cli)?, SERVICE_ACCOUNT, applied_root());
    let passed = findings.iter().all(|finding| finding.ok);
    if cli.json {
        print_json(&DoctorReport {
            schema: DoctorSchema,
            version: 1,
            passed,
            findings,
        })?;
    } else {
        for finding in &findings {
            match &finding.fix {
                None => println!("ok: {}: {}", finding.check, finding.detail),
                Some(fix) => {
                    println!("problem: {}: {}", finding.check, finding.detail);
                    println!("  fix: {fix}");
                }
            }
        }
    }
    Ok(ExitCode::from(if passed {
        exit::SUCCESS
    } else {
        exit::FAILURE
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_a_person_would_write_them() {
        assert_eq!(size(512 << 20), "512 MiB");
        assert_eq!(size(3 << 30), "3.0 GiB");
        assert_eq!(size((3 << 30) + (512 << 20)), "3.5 GiB");
    }
}
