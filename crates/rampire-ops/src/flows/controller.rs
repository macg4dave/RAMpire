//! The controller's systemd unit as the operator commands drive it: `init`
//! sets a host up and leaves the controller enabled and running, and
//! `recover` clears a VM's latch around a stop and start of the unit.

use std::path::Path;

use super::apply::apply;
use super::Outcome;
use crate::context::{applied_root, Context, SERVICE_ACCOUNT};
use crate::process;
use crate::ui::Ui;

/// This host's memory and its libvirt domains' memory, as `init` reads
/// them to suggest the global settings.
pub fn host_facts(context: &Context) -> Result<(u64, Vec<crate::init::DomainMemory>), String> {
    let host = crate::host_readings::read_meminfo()?.total;
    let libvirt = context.libvirt()?;
    let names = |state: &str| -> Result<Vec<String>, String> {
        Ok(libvirt
            .virsh(&["list", state, "--name"])?
            .lines()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect())
    };
    // Any domain but a shut-off one holds its memory: running, paused,
    // shutting down.
    let off = names("--state-shutoff")?;
    let mut domains = Vec::new();
    for name in names("--all")? {
        domains.push(crate::init::parse_domain(
            &name,
            &libvirt.virsh(&["dumpxml", "--inactive", &name])?,
            !off.contains(&name),
        )?);
    }
    Ok((host, domains))
}

/// After VMs were added or removed: offer the pool `init` would suggest
/// now, with the same reasoning, when it differs from the one set. A
/// problem here is said, not failed: the VM's own change is done.
pub fn offer_pool(context: &Context, ui: &mut dyn Ui) {
    if let Err(error) = offer_pool_or_fail(context, ui) {
        ui.say(&format!(
            "couldn't work out a new pool ({error}); rampire set global pool=SIZE sets it"
        ));
    }
}

fn offer_pool_or_fail(context: &Context, ui: &mut dyn Ui) -> Result<(), String> {
    use rampire_core::format_byte_size;
    if !is_set_up(context) {
        return Ok(());
    }
    let globals = rampire_core::settings::load_globals(&context.draft())?;
    let (host, domains) = host_facts(context)?;
    let Some(pool) = crate::init::new_pool(host, &domains, globals.host_reserve, globals.pool)
    else {
        return Ok(());
    };
    let now = globals
        .pool
        .map_or_else(|| "not set".to_owned(), format_byte_size);
    let mut shown = format!(
        "The pool can change: it is {now}, and from this host's {:.1} GiB, less the {} reserve, less what each VM needs whatever happens, {} is left:\n\n",
        host as f64 / rampire_core::GIB as f64,
        format_byte_size(globals.host_reserve),
        format_byte_size(pool),
    );
    for line in crate::init::set_aside_lines(&domains) {
        shown.push_str(&format!("  {line}\n"));
    }
    ui.show(&shown);
    if !ui.confirm(&format!(
        "Change the pool from {now} to {}?",
        format_byte_size(pool)
    ))? {
        ui.say("the pool is unchanged; rampire set global pool=SIZE changes it");
        return Ok(());
    }
    // Agreed once: written and applied without asking again.
    let pool = format_byte_size(pool);
    let change = crate::set::parse_assignment(&format!("pool={pool}"))?;
    let path = crate::set::set(&context.draft(), crate::set::GLOBAL, &[change])?;
    ui.say(&format!("changed {}", path.display()));
    crate::audit::record(
        context,
        "set",
        None,
        &[("changes", &format!("pool={pool}"))],
    );
    super::apply::apply_agreed(context, ui).map_err(|error| {
        format!(
            "{error}
the new pool is saved in {} but not applied: rampire apply applies it",
            path.display()
        )
    })?;
    Ok(())
}

/// Whether this host is set up: its global settings are written.
pub fn is_set_up(context: &Context) -> bool {
    context
        .draft()
        .join(rampire_core::settings::GLOBAL_FILE)
        .exists()
}

/// `rampire init`, keeping `host_reserve` for the host: as given, else
/// asked, with the recommended reserve as the answer to keep.
/// [`Outcome::Done`] when the host's settings are applied and the
/// controller runs them.
pub fn init(
    context: &Context,
    ui: &mut dyn Ui,
    host_reserve: Option<&str>,
) -> Result<Outcome, String> {
    use crate::init;
    let connection = context.connection()?;
    let draft = context.draft();
    // The host checks `doctor` makes, all but the applied configuration
    // init is about to make, so a host that can't run the controller is
    // told why before anything is written.
    let problems: Vec<_> = crate::doctor::run(&connection, SERVICE_ACCOUNT, applied_root())
        .into_iter()
        .filter(|finding| {
            !finding.ok
                && finding.check != crate::doctor::APPLIED_CHECK
                && finding.check != crate::doctor::CONTROLLER_CHECK
        })
        .collect();
    if !problems.is_empty() {
        for finding in &problems {
            ui.say(&format!("problem: {}: {}", finding.check, finding.detail));
            if let Some(fix) = &finding.fix {
                ui.say(&format!("  fix: {fix}"));
            }
        }
        return Err(
            "this host isn't ready for RAMpire; fix the problems above and run init again"
                .to_owned(),
        );
    }
    let (host, domains) = host_facts(context)?;
    let global = draft.join(rampire_core::settings::GLOBAL_FILE);
    let recommended = rampire_core::format_byte_size(init::recommended_reserve(host));
    let reserve = match host_reserve {
        Some(reserve) => reserve.trim().to_owned(),
        None if global.exists() => recommended,
        None => ui.edit(
            &format!(
                "Memory the host keeps for itself, of its {:.1} GiB (recommended {recommended}):",
                host as f64 / rampire_core::GIB as f64
            ),
            &recommended,
        )?,
    };
    let reserve = rampire_core::parse_byte_size(&reserve)
        .map_err(|error| format!("host reserve {reserve}: {error}"))?;
    if let Some(problem) = init::reserve_problem(host, reserve) {
        return Err(format!(
            "host reserve {}: {problem}; nothing was written",
            rampire_core::format_byte_size(reserve)
        ));
    }
    let suggestion = init::with_reserve(host, &domains, reserve);
    let text = init::global_toml(&suggestion, &domains);
    for folder in [
        rampire_core::settings::VMS_DIRECTORY,
        crate::applied::REVIEW_DIRECTORY,
        rampire_host::config::ATTESTATION_DIRECTORY,
    ] {
        let path = draft.join(folder);
        std::fs::create_dir_all(&path)
            .map_err(|error| format!("create {}: {error}", path.display()))?;
    }
    let global = draft.join(rampire_core::settings::GLOBAL_FILE);
    let existed = global.exists();
    if existed {
        ui.say(&format!(
            "{} already exists and is unchanged. Suggested from this host:\n\n{text}",
            global.display()
        ));
        ui.say("Change a value with: rampire set global KEY=VALUE");
    } else {
        std::fs::write(&global, &text)
            .map_err(|error| format!("write {}: {error}", global.display()))?;
        ui.say(&format!("wrote {}:\n\n{text}", global.display()));
        crate::audit::record(context, "init", None, &[]);
    }
    if suggestion.pool_bytes.is_none() {
        ui.say("This host has no memory left for a pool once its VMs have theirs.");
    }
    let applied = apply(context, ui)?;
    if let Outcome::Declined(_) = applied {
        return Ok(Outcome::Partly(format!(
            "{} is written but not applied, so the controller isn't started: rampire apply applies it",
            global.display()
        )));
    }
    // The unit runs the default configuration, so only that one starts it.
    let started = context.config.is_none() && start_controller(ui)?;
    Ok(match applied {
        Outcome::AlreadySo(_) if existed && !started => {
            Outcome::AlreadySo("this host is set up already; its settings are unchanged".to_owned())
        }
        _ => Outcome::Done,
    })
}

/// Enable and start the controller once a configuration is applied, so
/// `init` leaves it running and starting at boot. One already enabled and
/// running is left alone: `apply` has moved it onto the new copy. Whether
/// it was started.
fn start_controller(ui: &mut dyn Ui) -> Result<bool, String> {
    let unit = controller_unit();
    if crate::applied::current(applied_root())?.is_none() {
        ui.say(&format!("nothing is applied, so {unit} is not started"));
        return Ok(false);
    }
    let state = crate::systemd_unit::controller_state();
    if state.active_state == "active" && state.unit_file_state == "enabled" {
        return Ok(false);
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
    ui.say(&format!("started {unit}; it also starts at boot"));
    Ok(true)
}

pub fn controller_unit() -> String {
    crate::systemd_unit::CONTROLLER_UNIT.to_owned()
}

/// `rampire recover`: clear a VM's latch once a person has checked it is
/// safe to resume.
pub fn recover(
    context: &Context,
    ui: &mut dyn Ui,
    vm: &str,
    reason: &str,
) -> Result<Outcome, String> {
    let unit = controller_unit();
    let config = context.running();
    let config = config
        .to_str()
        .ok_or_else(|| format!("{} is not valid UTF-8", config.display()))?
        .to_owned();
    let connection = context.connection()?;
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
            .output()
            .map_err(|error| format!("run clear-latch as {SERVICE_ACCOUNT}: {error}"))
            .and_then(|output| {
                let text = String::from_utf8_lossy(&output.stdout).into_owned();
                if output.status.success() {
                    Ok(text)
                } else {
                    Err(format!(
                        "clear-latch failed with {}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    ))
                }
            })
    };
    // Show the VM as the controller sees it; clearing waits for the
    // controller to stop, since only one may hold the VM.
    let mut bytes = Vec::new();
    rampire_host::operator_actions::status(&context.target(Some(vm))?, &mut bytes)?;
    let status = rampire_core::parse_controller_status(&bytes)
        .map_err(|error| format!("status of {vm}: {error}"))?;
    ui.say(&crate::reports::status_line(&status));
    if !ui.confirm(&format!(
        "Stop {unit}, clear {vm}'s latch and start it again?"
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::Declined(format!(
            "nothing was changed: {vm} stays latched"
        )));
    }
    systemctl("stop", &unit)?;
    let cleared = clear_latch();
    // Start it again however recovery ended: a still-latched controller
    // only observes.
    let started = systemctl("start", &unit);
    let cleared = cleared?;
    if !cleared.is_empty() {
        ui.show(&cleared);
    }
    started?;
    ui.say(&format!("{vm}: latch cleared; {unit} started"));
    crate::audit::record(context, "recover", Some(vm), &[("reason", reason)]);
    Ok(Outcome::Done)
}

pub fn systemctl(action: &str, unit: &str) -> Result<(), String> {
    process::bounded_text(
        "systemctl",
        &[process::os(action), process::os(unit)],
        Path::new("/"),
        std::time::Duration::from_secs(120),
    )
    .map(|_| ())
}
