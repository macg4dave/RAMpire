//! The controller's systemd unit as the operator commands drive it: `init`
//! sets a host up and leaves the controller enabled and running, `logs`
//! shows its journal, and `recover` clears a VM's latch around a stop and
//! start of the unit.

use std::path::Path;
use std::process::ExitCode;

use rampire_ops::process;

use crate::apply::apply;
use crate::reporting::status_line;
use crate::{
    applied_root, confirm, connection, draft, path_text, running, target, Cli, SERVICE_ACCOUNT,
};

pub(crate) fn init(cli: &Cli) -> Result<ExitCode, String> {
    use rampire_ops::init;
    let connection = connection(cli)?;
    // The host checks `doctor` makes, all but the applied configuration
    // init is about to make, so a host that can't run the controller is
    // told why before anything is written.
    let problems: Vec<_> = rampire_ops::doctor::run(&connection, SERVICE_ACCOUNT, applied_root())
        .into_iter()
        .filter(|finding| !finding.ok && finding.check != rampire_ops::doctor::APPLIED_CHECK)
        .collect();
    if !problems.is_empty() {
        for finding in &problems {
            println!("problem: {}: {}", finding.check, finding.detail);
            if let Some(fix) = &finding.fix {
                println!("  fix: {fix}");
            }
        }
        return Err(
            "this host isn't ready for RAMpire; fix the problems above and run init again"
                .to_owned(),
        );
    }
    let host = rampire_ops::watch::read_meminfo()?.total;
    let timeout = std::time::Duration::from_secs(30);
    let virsh = |args: &[&str]| {
        let mut arguments = vec![process::os("-c"), process::os(&connection)];
        arguments.extend(args.iter().map(process::os));
        process::bounded_text("virsh", &arguments, Path::new("/"), timeout)
    };
    let mut domains = Vec::new();
    for name in virsh(&["list", "--all", "--name"])?.lines().map(str::trim) {
        if !name.is_empty() {
            domains.push(init::parse_domain(
                name,
                &virsh(&["dumpxml", "--inactive", name])?,
            )?);
        }
    }
    let suggestion = init::suggest(host, &domains);
    let text = init::global_toml(&suggestion, &domains);
    for folder in [
        rampire_core::settings::VMS_DIRECTORY,
        rampire_ops::applied::REVIEW_DIRECTORY,
        rampire_host::config::ATTESTATION_DIRECTORY,
    ] {
        let path = draft(cli).join(folder);
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("create {}: {error}", path.display()))?;
    }
    let global = draft(cli).join(rampire_core::settings::GLOBAL_FILE);
    if global.exists() {
        println!(
            "{} already exists and is unchanged. Suggested from this host:

{text}",
            global.display()
        );
        println!("Change a value with: rampire set global KEY=VALUE");
    } else {
        std::fs::write(&global, &text)
            .map_err(|error| format!("write {}: {error}", global.display()))?;
        println!(
            "wrote {}:

{text}",
            global.display()
        );
    }
    if suggestion.pool_bytes.is_none() {
        println!("This host has no memory left for a pool once its VMs have theirs.");
    }
    let code = apply(cli, false)?;
    // The unit runs the default configuration, so only that one starts it.
    if cli.config.is_none() {
        start_controller()?;
    }
    Ok(code)
}

/// Enable and start the controller once a configuration is applied, so
/// `init` leaves it running and starting at boot. One already enabled and
/// running is left alone: `apply` has moved it onto the new copy.
fn start_controller() -> Result<(), String> {
    let unit = controller_unit();
    if rampire_ops::applied::current(applied_root())?.is_none() {
        println!("nothing is applied, so {unit} is not started");
        return Ok(());
    }
    let state = rampire_ops::systemd_unit::controller_state();
    if state.active_state == "active" && state.unit_file_state == "enabled" {
        return Ok(());
    }
    process::bounded_text(
        "systemctl",
        &[
            process::os("enable"),
            process::os("--now"),
            process::os(&unit),
        ],
        Path::new("/"),
        std::time::Duration::from_secs(120),
    )?;
    println!("started {unit}; it also starts at boot");
    Ok(())
}

pub(crate) fn controller_unit() -> String {
    rampire_ops::systemd_unit::CONTROLLER_UNIT.to_owned()
}

pub(crate) fn logs(follow: bool, since: Option<&str>) -> Result<ExitCode, String> {
    let mut command = std::process::Command::new("journalctl");
    command.args(["--unit", &controller_unit(), "--no-pager"]);
    if follow {
        command.arg("--follow");
    }
    if let Some(since) = since {
        command.args(["--since", since]);
    }
    let status = command
        .status()
        .map_err(|error| format!("run journalctl: {error}"))?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

pub(crate) fn recover(cli: &Cli, vm: &str, reason: &str) -> Result<ExitCode, String> {
    let unit = controller_unit();
    let config = path_text(&running(cli))?;
    let connection = connection(cli)?;
    let executable =
        std::env::current_exe().map_err(|error| format!("find the rampire executable: {error}"))?;
    let clear_latch = || {
        let mut command = std::process::Command::new("runuser");
        command
            .args(["-u", SERVICE_ACCOUNT, "--"])
            .arg(&executable)
            .args([
                "--config",
                &config,
                "--connect",
                &connection,
                "clear-latch",
                vm,
                reason,
            ]);
        command.arg("--apply");
        command
            .status()
            .map_err(|error| format!("run clear-latch as {SERVICE_ACCOUNT}: {error}"))
            .and_then(|status| {
                status
                    .success()
                    .then_some(())
                    .ok_or_else(|| format!("clear-latch failed with {status}"))
            })
    };
    // Show the VM as the controller sees it; clearing waits for the
    // controller to stop, since only one may hold the VM.
    let mut bytes = Vec::new();
    rampire_host::operator_actions::status(&target(cli, Some(vm))?, &mut bytes)?;
    let status = rampire_core::parse_controller_status(&bytes)
        .map_err(|error| format!("status of {vm}: {error}"))?;
    println!("{}", status_line(&status));
    if !confirm(
        cli,
        &format!("Stop {unit}, clear {vm}'s latch and start it again?"),
    )? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    systemctl("stop", &unit)?;
    let cleared = clear_latch();
    // Start it again however recovery ended: a still-latched controller
    // only observes.
    let started = systemctl("start", &unit);
    cleared?;
    started?;
    println!("{vm}: latch cleared; {unit} started");
    Ok(ExitCode::SUCCESS)
}

pub(crate) fn systemctl(action: &str, unit: &str) -> Result<(), String> {
    process::bounded_text(
        "systemctl",
        &[process::os(action), process::os(unit)],
        Path::new("/"),
        std::time::Duration::from_secs(120),
    )
    .map(|_| ())
}
