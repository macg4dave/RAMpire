//! `rampire vm upgrade`, and the waits for a guest service to report in
//! that it and `vm add` share, saying what they see and, when it doesn't,
//! why.

use super::*;

/// Wait for the guest service to report in, and say how it is. While it
/// waits it says what it sees every [`WAIT_REPORT`], so a long wait isn't
/// silent; when the wait ends without it, why, as far as the host can tell.
pub(super) fn guest_reports_in(
    context: &Context,
    libvirt: &crate::attest::Libvirt,
    ui: &mut dyn Ui,
    vm: &str,
    limit: std::time::Duration,
) -> Result<(), String> {
    use std::time::{Duration, Instant};
    ui.say(&format!(
        "waiting up to {} s for {vm}'s guest service to report in",
        limit.as_secs()
    ));
    let started = Instant::now();
    let mut reported = started;
    let cancel = ui.cancel();
    while started.elapsed() < limit {
        if let Ok(status) = crate::inventory::read_status(context, vm) {
            if status.guest_input.observed_unix_millis.is_some() {
                ui.say(&format!(
                    "{vm} is added: {}",
                    crate::reports::status_line(&status, crate::vm::boot_bytes(libvirt, vm))
                ));
                return Ok(());
            }
        }
        if reported.elapsed() >= WAIT_REPORT {
            reported = Instant::now();
            ui.say(&format!(
                "still waiting, {} s of {}: {}",
                started.elapsed().as_secs(),
                limit.as_secs(),
                why_not_reported(context, libvirt, vm)
            ));
        }
        if cancel.sleep(Duration::from_secs(2)) {
            return Err(format!(
                "(you stopped waiting): {}",
                why_not_reported(context, libvirt, vm)
            ));
        }
    }
    Err(format!(
        "after {} s: {}",
        limit.as_secs(),
        why_not_reported(context, libvirt, vm)
    ))
}

/// How often a wait for a guest says what it sees.
const WAIT_REPORT: std::time::Duration = std::time::Duration::from_secs(20);

/// Why `vm`'s guest service hasn't reported in, as far as the host can
/// tell: the first cause found, in the order the VM depends on them. The
/// controller runs; the VM runs; it runs its saved definition; its guest
/// service runs without an error; the controller reaches its control
/// channel.
fn why_not_reported(context: &Context, libvirt: &crate::attest::Libvirt, vm: &str) -> String {
    let controller = crate::doctor::controller_finding(&crate::systemd_unit::controller_state());
    if !controller.ok {
        return format!(
            "{}. Fix: {}",
            controller.detail,
            controller.fix.unwrap_or_default()
        );
    }
    match libvirt.state(vm) {
        Ok(state) if state == "running" => {}
        Ok(state) => return format!("{vm} is {state}; it reports in once it runs"),
        Err(error) => return format!("libvirt can't say how {vm} is: {error}"),
    }
    if crate::vm::runs_behind(libvirt, vm) == Some(true) {
        return format!(
            "{vm} still runs its old definition, without RAMpire's device and channel: it needs a restart (rampire vm restart {vm})"
        );
    }
    let service = match crate::guest::service_state(libvirt, vm) {
        Some(service) => service,
        None => {
            return format!(
                "its guest agent doesn't run commands, so RAMpire can't see inside {vm}. In {vm}: systemctl status rampire-guest"
            )
        }
    };
    if let Some(error) = &service.error {
        return format!(
            "its guest service {}: {error}. In {vm}: journalctl -u rampire-guest",
            service.described()
        );
    }
    if service.state != "active" {
        return format!(
            "its guest service {} ({}). In {vm}: systemctl status rampire-guest",
            service.described(),
            service.state
        );
    }
    // The guest is up: what is missing is between it and the controller.
    // The controller says why it can't reach the channel; a guest without
    // one writes control files instead, which the controller reads through
    // the agent, so its error about those files says nothing here.
    match crate::journal::channel_event(vm, Path::new("/")) {
        Some(event) if event.starts_with("unavailable") => {
            return format!(
                "its guest service runs, but the controller can't reach {vm}'s control channel: {}",
                event.trim_start_matches("unavailable: ")
            )
        }
        Some(_) => {
            return "its guest service runs and the controller is connected to its control channel, but nothing has come over it yet".to_owned()
        }
        None => {}
    }
    match crate::inventory::read_status(context, vm) {
        Ok(status) => {
            let problems: Vec<String> = crate::doctor_vm::controller(vm, &status, None)
                .into_iter()
                .filter(|finding| !finding.ok)
                .map(|finding| finding.detail)
                .collect();
            if problems.is_empty() {
                format!(
                    "its guest service runs, but the controller has had nothing from it yet and hasn't said why: rampire logs --vm {vm}"
                )
            } else {
                problems.join("; ")
            }
        }
        Err(error) => format!(
            "its guest service runs, but the controller has no status for {vm}: {error}. rampire status says how the controller is"
        ),
    }
}

/// `rampire vm upgrade`: install the guest package of the build this host
/// runs in a managed VM, logged in to it as `vm add` does; when it can't be
/// logged in to, and that is agreed to, on its disk with it shut down for a
/// moment. Then wait for the new build to report in.
pub fn vm_upgrade(
    context: &Context,
    ui: &mut dyn Ui,
    vm: &str,
    login: Option<&str>,
) -> Result<Outcome, String> {
    crate::attest::vm_settings(&context.draft(), vm)?;
    let host = context.build_id();
    let running = crate::guest::running_build(vm);
    if let (Some(guest), Some(host)) = (running.as_deref(), host) {
        if guest == host {
            let same = format!("{vm} runs rampire-guest {host} already, the build this host runs");
            ui.say(&same);
            return Ok(Outcome::AlreadySo(same));
        }
    }
    let from = running.map_or("the build it runs".to_owned(), |build| {
        format!("build {build}")
    });
    let to = host.map_or("the build this host runs".to_owned(), |host| {
        format!("build {host}, the one this host runs")
    });
    if !ui.confirm(&format!(
        "Upgrade rampire-guest in {vm} from {from} to {to}?"
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let libvirt = context.libvirt()?;
    // The TUI has no SSH login: it doesn't ask for one.
    let ask_login = context.via != "tui";
    if !upgrade_guest(ui, &libvirt, vm, login, true, ask_login)? {
        return Ok(Outcome::Declined(format!(
            "nothing was changed: {vm} keeps the guest build it runs"
        )));
    }
    crate::audit::record(context, "vm_upgrade", Some(vm), &[]);
    Ok(
        match wait_for_build(context, &libvirt, ui, vm, host, std::time::Duration::from_secs(300)) {
            Ok(()) => Outcome::Done,
            Err(why) => Outcome::Partly(format!(
                "the package is installed, but {vm}'s guest service hasn't reported in from the new build {why}"
            )),
        },
    )
}

/// Install the guest package of this host's build in `vm`: logged in to it
/// while it runs; else, when `may_shut_down` and it is agreed to, on its
/// disk with it shut down for a moment, the package installing as it starts
/// again. A VM that is off gets it on its disk without asking. When it
/// can't be logged in to by itself, its login is asked first, when `ask_login`. `false` when
/// the person answered No to shutting it down: nothing was changed.
fn upgrade_guest(
    ui: &mut dyn Ui,
    libvirt: &crate::attest::Libvirt,
    vm: &str,
    login: Option<&str>,
    may_shut_down: bool,
    ask_login: bool,
) -> Result<bool, String> {
    use std::time::Duration;
    let offline = |running: bool, ui: &mut dyn Ui| -> Result<bool, String> {
        ui.say(&format!(
            "putting rampire-guest on {vm}'s disk; it installs as {vm} starts"
        ));
        crate::guest::install_offline(libvirt, vm, running)?;
        libvirt.virsh(&["start", vm])?;
        ui.say(&format!("{vm} started"));
        Ok(true)
    };
    if libvirt.state(vm)? != "running" {
        return offline(false, ui);
    }
    let install = |ui: &mut dyn Ui, login: Option<&str>| {
        crate::guest::Guest::reach(libvirt, vm, login, Duration::from_secs(60), &ui.cancel())
            .and_then(|guest| {
                ui.say(&format!("installing rampire-guest in {vm} {}", guest.way()));
                guest.install()
            })
    };
    let mut logged_in = install(ui, login);
    // The login `vm add` was given isn't kept: ask for one before
    // anything that shuts the VM down, except in the TUI, which has none.
    if let (Err(why), None, true) = (&logged_in, login, ask_login) {
        if !ui.cancel().is_set() {
            ui.say(why);
            let given = ui.edit(
                &format!("{vm}'s SSH login, USER@HOST, to install with (empty: none)"),
                "",
            )?;
            let given = given.trim();
            if !given.is_empty() {
                logged_in = install(ui, Some(given));
            }
        }
    }
    match logged_in {
        Ok(installed) => {
            ui.say(&format!("{vm} has rampire-guest {}", installed.build));
            if installed.restart_needed {
                // The package turned pressure stall information on for the
                // next start and leaves the service stopped until then.
                ui.say(
                    "the package turned on pressure stall information, which takes a restart; its guest service starts then",
                );
                if may_shut_down
                    && ui.confirm(&format!(
                        "Shut {vm} down and start it again now, so its guest service runs?"
                    ))?
                {
                    ui.say(&format!("shutting {vm} down and starting it again"));
                    crate::attest::cold_restart(vm, libvirt, Duration::from_secs(300))?;
                    ui.say(&format!("{vm} restarted"));
                } else {
                    ui.say(&format!("rampire vm restart {vm} restarts it"));
                }
            }
            Ok(true)
        }
        Err(why) => {
            if !may_shut_down || !crate::guest::offline_available() || ui.cancel().is_set() {
                return Err(format!("rampire-guest in {vm} wasn't upgraded: {why}"));
            }
            ui.say(&why);
            if !ui.confirm(&format!(
                "Shut {vm} down for a moment to put the package on its disk?"
            ))? {
                ui.say("nothing changed");
                return Ok(false);
            }
            offline(true, ui)
        }
    }
}

/// Wait for `vm`'s guest service to publish from `build`, saying what it
/// sees every [`WAIT_REPORT`] as `vm add`'s wait does; when it doesn't,
/// why. Without a build to wait for, there is nothing to wait on.
fn wait_for_build(
    context: &Context,
    libvirt: &crate::attest::Libvirt,
    ui: &mut dyn Ui,
    vm: &str,
    build: Option<&str>,
    limit: std::time::Duration,
) -> Result<(), String> {
    use std::time::{Duration, Instant};
    let Some(build) = build else {
        return Ok(());
    };
    ui.say(&format!(
        "waiting up to {} s for {vm}'s guest service to report in from build {build}",
        limit.as_secs()
    ));
    let started = Instant::now();
    let mut reported = started;
    let cancel = ui.cancel();
    let running_now = || {
        crate::guest::running_build(vm).map_or(String::new(), |old| {
            format!("it still reports build {old}; ")
        })
    };
    while started.elapsed() < limit {
        if crate::guest::running_build(vm).as_deref() == Some(build) {
            ui.say(&format!("{vm} runs rampire-guest {build}"));
            return Ok(());
        }
        if reported.elapsed() >= WAIT_REPORT {
            reported = Instant::now();
            ui.say(&format!(
                "still waiting, {} s of {}: {}{}",
                started.elapsed().as_secs(),
                limit.as_secs(),
                running_now(),
                why_not_reported(context, libvirt, vm)
            ));
        }
        if cancel.sleep(Duration::from_secs(2)) {
            return Err(format!(
                "(you stopped waiting): {}{}",
                running_now(),
                why_not_reported(context, libvirt, vm)
            ));
        }
    }
    Err(format!(
        "after {} s: {}{}",
        limit.as_secs(),
        running_now(),
        why_not_reported(context, libvirt, vm)
    ))
}

/// After a VM's settings change: when its guest runs another build than
/// this host's, upgrade it while it runs. One that can't be logged in to is
/// left for `rampire vm upgrade`, which can shut it down for a moment.
pub(super) fn upgrade_if_behind(
    context: &Context,
    ui: &mut dyn Ui,
    vm: &str,
) -> Result<(), String> {
    let host = context.build_id();
    let publication =
        crate::top::read_publication(&rampire_host::guest_channel::publication_copy_path(vm));
    if !crate::guest::update_available(host, publication.as_ref()) {
        return Ok(());
    }
    let libvirt = context.libvirt()?;
    if libvirt.state(vm)? != "running" {
        return Ok(());
    }
    ui.say(&format!(
        "{vm} runs rampire-guest {}; upgrading it to this host's build",
        crate::guest::publication_build(publication.as_ref()).unwrap_or("of another build")
    ));
    match upgrade_guest(ui, &libvirt, vm, None, false, context.via != "tui") {
        Ok(_) => {
            crate::audit::record(context, "vm_upgrade", Some(vm), &[]);
            if let Err(why) = wait_for_build(
                context,
                &libvirt,
                ui,
                vm,
                host,
                std::time::Duration::from_secs(120),
            ) {
                ui.say(&format!(
                    "{vm}'s guest service hasn't reported in from the new build {why}"
                ));
            }
        }
        Err(why) => ui.say(&format!(
            "{why}; rampire vm upgrade {vm} does it, shutting {vm} down for a moment if it must"
        )),
    }
    Ok(())
}
