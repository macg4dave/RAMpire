//! The read-only reports: `status`, `check`, `doctor` and `vm list`, as
//! text for a person or as a versioned JSON document with `--json`.

use std::process::ExitCode;

use rampire_host::config::HostSettings;
use rampire_ops::context::{applied_root, SERVICE_ACCOUNT};
use rampire_ops::reports::{
    status_line, DoctorReport, DoctorSchema, StatusReport, StatusReportSchema, REPORT_VERSION,
};
use serde::Serialize;

use crate::{context, exit, Cli};

pub(crate) fn status(cli: &Cli, vm: Option<&str>) -> Result<ExitCode, String> {
    let context = context(cli);
    let names = match vm {
        Some(vm) => vec![vm.to_owned()],
        None => HostSettings::load(&context.running())
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
        match rampire_ops::inventory::read_status(&context, name) {
            Ok(status) => vms.push(status),
            Err(reason) => unavailable.push(rampire_ops::reports::UnavailableVm {
                vm_name: name.clone(),
                state: context
                    .libvirt()
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
            version: REPORT_VERSION,
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
        println!("RAMpire manages no VMs yet. Add one with: rampire vm add VM");
    }
    for status in &vms {
        let boot = context
            .libvirt()
            .ok()
            .and_then(|libvirt| rampire_ops::vm::boot_bytes(&libvirt, &status.vm_name));
        println!("{}", status_line(status, boot));
    }
    Ok(ExitCode::SUCCESS)
}

fn print_json<T: Serialize>(document: &T) -> Result<(), String> {
    let text = serde_json::to_string_pretty(document).map_err(|error| error.to_string())?;
    println!("{text}");
    Ok(())
}

pub(crate) fn check(cli: &Cli) -> Result<ExitCode, String> {
    let report = rampire_ops::check::run(&context(cli).draft())?;
    if cli.json {
        print_json(&report)?;
    } else {
        print!("{}", rampire_ops::check::text(&report, false));
    }
    Ok(ExitCode::from(if report.passed {
        exit::SUCCESS
    } else {
        exit::FAILURE
    }))
}

/// `doctor`: the host's checks and a line per managed VM, or with a VM
/// everything about it.
pub(crate) fn doctor(cli: &Cli, vm: Option<&str>) -> Result<ExitCode, String> {
    let context = context(cli);
    let findings = match vm {
        Some(vm) => rampire_ops::doctor_vm::run(&context, vm)?,
        None => {
            let mut findings =
                rampire_ops::doctor::run(&context.connection()?, SERVICE_ACCOUNT, applied_root());
            findings.extend(rampire_ops::doctor_vm::summaries(&context));
            findings
        }
    };
    let passed = findings.iter().all(|finding| finding.ok);
    if cli.json {
        print_json(&DoctorReport {
            schema: DoctorSchema,
            version: REPORT_VERSION,
            vm: vm.map(str::to_owned),
            passed,
            findings,
        })?;
    } else {
        print!("{}", rampire_ops::doctor::text(&findings));
    }
    Ok(ExitCode::from(if passed {
        exit::SUCCESS
    } else {
        exit::FAILURE
    }))
}

/// `vm list`.
pub(crate) fn vm_list(cli: &Cli) -> Result<ExitCode, String> {
    let report = rampire_ops::inventory::list(&context(cli))?;
    if cli.json {
        print_json(&report)?;
    } else {
        print!("{}", rampire_ops::inventory::text(&report));
    }
    Ok(ExitCode::SUCCESS)
}
