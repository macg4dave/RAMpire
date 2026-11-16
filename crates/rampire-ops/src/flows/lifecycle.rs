//! `vm add`, `vm remove` and `vm restart`: bringing a VM under management
//! (fixing its definition, installing its guest package, writing its review
//! and settings), letting it go with everything RAMpire kept for it, and
//! restarting it onto its saved definition.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::apply::{apply, running_copy};
use super::Outcome;
use crate::context::{applied_root, Context, TempDir, DOMAIN_BACKUPS};
use crate::process;
use crate::ui::Ui;
use rampire_core::format_byte_size;
use rampire_host::attestation::AnswerSource;

/// `vm add`'s options beside the VM's name.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub vm: String,
    pub review: Option<String>,
    pub kernel: Option<String>,
    pub priority: u32,
    pub min_ram: Option<String>,
    pub max_ram: Option<String>,
    pub free_ram: Option<String>,
    /// `USER@HOST` to log in to the guest with, to install its package.
    pub guest_login: Option<String>,
    /// The guest package is installed some other way.
    pub no_guest_install: bool,
}

impl AddOptions {
    /// `vm add VM`, with every default.
    pub fn new(vm: &str) -> Self {
        Self {
            vm: vm.to_owned(),
            review: None,
            kernel: None,
            priority: 100,
            min_ram: None,
            max_ram: None,
            free_ram: None,
            guest_login: None,
            no_guest_install: false,
        }
    }

    fn has_settings(&self) -> bool {
        self.review.is_some()
            || self.kernel.is_some()
            || self.priority != 100
            || self.min_ram.is_some()
            || self.max_ram.is_some()
            || self.free_ram.is_some()
    }
}

/// Write a file that must not exist yet; an existing one is never replaced.
fn write_new(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => format!("{} already exists", path.display()),
            _ => format!("write {}: {error}", path.display()),
        })?;
    file.write_all(text.as_bytes())
        .map_err(|error| format!("write {}: {error}", path.display()))
}

/// Where a VM's settings are in the draft.
fn settings_path(directory: &Path, name: &str) -> Result<PathBuf, String> {
    Ok(directory
        .join(rampire_core::settings::VMS_DIRECTORY)
        .join(rampire_core::settings::vm_filename(name)?))
}

/// `rampire vm add`. Shows everything it will do and asks once; then fixes
/// the definition, starts the VM, installs the guest package in it,
/// restarts it when that is needed, writes its settings, applies, and waits
/// for the guest to report in. [`Outcome::Partly`] when the VM is managed
/// but its guest service hasn't reported in.
pub fn vm_add(context: &Context, ui: &mut dyn Ui, options: &AddOptions) -> Result<Outcome, String> {
    use crate::vm;
    use rampire_core::settings::VMS_DIRECTORY;
    use std::time::Duration;
    let name = options.vm.as_str();
    let directory = context.draft();
    let filename = rampire_core::settings::vm_filename(name)?;
    let settings_path = settings_path(&directory, name)?;
    // Settings in the draft but not in the applied copy are an earlier
    // `vm add` that stopped before applying: this run finishes it.
    let resuming = settings_path.exists();
    if resuming {
        let applied = crate::applied::current(applied_root())?
            .map(|current| current.join(VMS_DIRECTORY).join(&filename));
        if applied.is_some_and(|path| path.exists()) {
            return Err(format!(
                "{name} is already managed; change it with rampire set {name} KEY=VALUE"
            ));
        }
    }
    let kept = resuming;
    if kept && options.has_settings() {
        return Err(format!(
            "{name} has settings already ({}): add it without settings, then change them with rampire set {name} KEY=VALUE",
            settings_path.display()
        ));
    }
    let libvirt = context.libvirt()?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
    let inspection = vm::inspect(&xml)?;
    let fixed = vm::fix_xml(&xml, &inspection)?;
    let after = match &fixed {
        Some(fixed) => vm::inspect(fixed)?,
        None => inspection.clone(),
    };
    let problems = after.problems();
    if !problems.is_empty() {
        for (problem, fix) in &problems {
            ui.say(&format!("problem: {problem}"));
            ui.say(&format!("  fix: {fix}"));
        }
        return Err(format!("{name} can't be managed yet; nothing was changed"));
    }
    // Without problems there is one device, and it has an alias.
    let alias = after
        .device()
        .and_then(|device| device.alias.clone())
        .ok_or_else(|| format!("{name} has no virtio-mem device with an alias"))?;

    // `apply` checks every VM: one that fails would stop it at the end.
    other_vms_pass(context, &libvirt, ui, name)?;

    // rampire-guest installs as a package, or as the static binary with its
    // own unit, through a login or on the disk: a system whose root is read
    // only (Home Assistant OS) takes neither, and is told so before
    // anything is changed.
    if !options.no_guest_install && crate::guest::offline_available() {
        ui.say(&format!("reading {name}'s disk to see what system it runs"));
        match crate::guest::inspect_disk(name) {
            Ok(system) if !system.takes_guest() => {
                let product = system
                    .product
                    .unwrap_or_else(|| "a system RAMpire doesn't recognise".to_owned());
                return Err(format!(
                    "{name} runs {product}, whose system is read only, so rampire-guest can't be installed in it; nothing was changed"
                ));
            }
            Ok(_) => {}
            // Not read: the install says what is wrong, if anything is.
            Err(error) => ui.say(&format!("couldn't read {name}'s disk ({error}); going on")),
        }
    }

    let running = libvirt.state(name)? == "running";
    let changes = inspection.fixable();
    // Every change but saving the requested size takes effect when the VM
    // next starts; a VM fixed by an earlier run may still run the old one.
    let mut restart = running && inspection.fixes_take_a_restart();
    if running && !restart {
        restart = after.runs_behind(&vm::inspect(&libvirt.virsh(&["dumpxml", name])?)?);
    }
    let settings = if kept {
        None
    } else {
        Some(chosen_settings(&after, options)?)
    };

    // What will be done, agreed to once.
    let mut shown = format!("Adding {name} to RAMpire\n\n");
    if restart {
        shown.push_str(&format!(
            "  ! {name} is running: it will be shut down and started again.\n\n"
        ));
    } else if running && !options.no_guest_install {
        shown.push_str(&format!(
            "  ! {name} is running: if RAMpire can't log in to it, it is shut down for a moment to install rampire-guest.\n\n"
        ));
    }
    let mut row = |label: &str, text: &str| shown.push_str(&format!("  {label:<11} {text}\n"));
    match &settings {
        Some(chosen) => row(
            "Settings",
            &format!(
                "priority {}, min {}, max {}, free {}",
                options.priority,
                format_byte_size(chosen.min_ram),
                format_byte_size(chosen.max_ram),
                format_byte_size(chosen.free_ram)
            ),
        ),
        None => row(
            "Settings",
            &format!(
                "kept from the earlier vm add, in {}",
                settings_path.display()
            ),
        ),
    }
    for (index, change) in changes.iter().enumerate() {
        row(if index == 0 { "Definition" } else { "" }, change);
    }
    if !options.no_guest_install {
        row("Guest", "install rampire-guest, this host's build");
    }
    if !running {
        row("Then", "start it");
    } else if restart {
        row("Then", "shut it down and start it again");
    }
    ui.show(&shown);
    let question = format!("Add {name}? RAMpire then grows and shrinks its memory while it runs.");
    // Agreeing answers the review's question about the guest's programs,
    // which only a person may answer: never `--yes`.
    let new_review = !kept && options.review.is_none();
    let agreed = if new_review {
        ui.review(&question)?
    } else {
        ui.confirm(&question)?
    };
    if !agreed {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let answers = if new_review {
        ask_review(ui, &after, name)?
    } else {
        Vec::new()
    };

    // What this run writes, so a failure can take it out again: files that
    // were there before it are never removed.
    let review_path = directory
        .join(crate::applied::REVIEW_DIRECTORY)
        .join(format!(
            "{}.toml",
            rampire_core::settings::vm_file_stem(name)
        ));
    let recorded_path = rampire_host::config::attestation_path(&directory, name);
    let mut undo = Undo {
        name,
        xml: &xml,
        backup: None,
        settings: (!resuming).then(|| settings_path.clone()),
        review: (new_review && !review_path.exists()).then_some(review_path),
        recorded: (!resuming && !recorded_path.exists()).then_some(recorded_path),
    };

    // Then it is done.
    if let Some(fixed) = &fixed {
        let backup = redefine(&libvirt, name, &xml, fixed)?;
        ui.say(&format!(
            "changed {name}'s definition; the old one is in {}",
            backup.display()
        ));
        crate::audit::record(
            context,
            "fix_xml",
            Some(name),
            &[("backup", &backup.display().to_string())],
        );
        undo.backup = Some(backup);
    }
    // What follows can fail with the definition changed: the error then
    // says how to go on or go back.
    #[allow(clippy::redundant_closure_call)]
    let finished = (|| -> Result<Outcome, String> {
        // The guest package: a VM that is off gets it on its disk before it
        // first starts; a running one through a login, else the same way with
        // it shut down for a moment. An install on the disk runs at the boot.
        let offline = |running: bool, ui: &mut dyn Ui| -> Result<crate::guest::Installed, String> {
            ui.say(&format!(
                "putting rampire-guest on {name}'s disk; it installs as {name} starts"
            ));
            let kernel = crate::guest::install_offline(&libvirt, name, running)?;
            libvirt.virsh(&["start", name])?;
            ui.say(&format!("{name} started"));
            Ok(crate::guest::Installed {
                kernel,
                build: String::new(),
                restart_needed: false,
            })
        };
        let mut installed = None;
        if !running && !options.no_guest_install && crate::guest::offline_available() {
            installed = Some(offline(false, ui)?);
        } else if !running {
            ui.say(&format!("starting {name}"));
            libvirt.virsh(&["start", name])?;
            ui.say(&format!("{name} started"));
        }
        if !options.no_guest_install && installed.is_none() {
            ui.say(&format!(
            "logging in to {name} to install rampire-guest; this waits up to 3 minutes for it to boot"
        ));
            let logged_in = crate::guest::Guest::reach(
                &libvirt,
                name,
                options.guest_login.as_deref(),
                Duration::from_secs(180),
                &ui.cancel(),
            )
            .and_then(|guest| {
                ui.say(&format!(
                    "installing rampire-guest in {name} {}",
                    guest.way()
                ));
                guest.install()
            });
            installed = Some(match logged_in {
                Ok(done) => {
                    ui.say(&format!(
                        "{name} has rampire-guest {}, on Linux {}",
                        done.build, done.kernel
                    ));
                    done
                }
                // A person who stopped the wait doesn't want the VM shut
                // down for the other way in.
                Err(why) if ui.cancel().is_set() => return Err(why),
                Err(why) => {
                    ui.say(&why);
                    // Shut down and started again: its new definition is in.
                    restart = false;
                    offline(true, ui)?
                }
            });
        }
        if restart
            || installed
                .as_ref()
                .is_some_and(|installed| installed.restart_needed)
        {
            ui.say(&format!("shutting {name} down and starting it again"));
            crate::attest::cold_restart(name, &libvirt, Duration::from_secs(300))?;
            ui.say(&format!("{name} restarted"));
        }

        if resuming {
            // Written by the earlier run; kept as it is.
        } else {
            let review = if new_review {
                let kernel = match &installed {
                    Some(installed) => (installed.kernel.clone(), AnswerSource::Detected),
                    None => kernel_release(ui, &libvirt, options, Duration::from_secs(90))?,
                };
                Some((answers, kernel))
            } else {
                None
            };
            let chosen = settings.ok_or("no settings were chosen")?;
            write_vm_settings(ui, &directory, &after, &alias, options, &chosen, review)?;
        }
        if !resuming {
            crate::audit::record(context, "vm_add", Some(name), &[]);
        }
        super::apply::apply_agreed(context, ui)?;
        // A VM still on its old definition can't be resized, and its guest
        // service has no channel to report on: it needs a cold start.
        if runs_behind(&libvirt, name) == Some(true) {
            ui.say(&format!(
                "{name} still runs its old definition, without RAMpire's device and channel"
            ));
            if ui.confirm(&format!(
                "Shut {name} down and start it again now, so RAMpire can resize it?"
            ))? {
                ui.say(&format!("shutting {name} down and starting it again"));
                crate::attest::cold_restart(name, &libvirt, Duration::from_secs(300))?;
                ui.say(&format!("{name} restarted"));
            } else {
                return Ok(Outcome::Partly(format!(
                    "{name} is managed, but runs its old definition until it restarts: rampire vm restart {name}"
                )));
            }
        }
        // An install at boot may boot the VM once more.
        Ok(
            match guest_reports_in(context, &libvirt, ui, name, Duration::from_secs(300)) {
                Ok(()) => Outcome::Done,
                Err(why) => Outcome::Partly(format!(
                    "{name} is managed, but its guest service hasn't reported in {why}"
                )),
            },
        )
    })();
    match finished {
        Err(error) if undo.has_work() => Err(undo.run(context, &libvirt, ui, error)),
        Ok(outcome) => {
            // Its RAM above the boot RAM is the pool's now.
            super::controller::offer_pool(context, ui);
            Ok(outcome)
        }
        finished => finished,
    }
}

/// What a failed `vm add` takes out again, so the VM is as it was before:
/// the definition it changed, and the files it wrote. The guest package
/// stays installed in the guest.
struct Undo<'a> {
    name: &'a str,
    /// The definition before this run changed it.
    xml: &'a str,
    /// The copy of `xml` this run kept, once it changed the definition.
    backup: Option<PathBuf>,
    /// Files this run may have written; none of them was there before it.
    settings: Option<PathBuf>,
    review: Option<PathBuf>,
    recorded: Option<PathBuf>,
}

impl Undo<'_> {
    /// Whether this run changed anything that is still there.
    fn has_work(&self) -> bool {
        self.backup.is_some()
            || [&self.settings, &self.review]
                .into_iter()
                .flatten()
                .any(|path| path.exists())
    }

    /// Remove the files this run wrote, and its copy of the definition.
    fn remove_written(&self, ui: &mut dyn Ui) -> Result<(), String> {
        for path in [&self.settings, &self.review, &self.recorded, &self.backup]
            .into_iter()
            .flatten()
        {
            match std::fs::remove_file(path) {
                Ok(()) => ui.say(&format!("removed {}", path.display())),
                Err(why) if why.kind() == std::io::ErrorKind::NotFound => {}
                Err(why) => return Err(format!("remove {}: {why}", path.display())),
            }
        }
        Ok(())
    }

    /// Put the VM back as it was and say so after `error`; when that can't
    /// be done, say how to go on or go back by hand.
    fn run(
        self,
        context: &Context,
        libvirt: &crate::attest::Libvirt,
        ui: &mut dyn Ui,
        error: String,
    ) -> String {
        let name = self.name;
        let by_hand = |why: String| {
            format!(
                "{error}\n{name} was partly added and couldn't be put back as it was ({why}): rampire vm add {name} tries again, and rampire vm remove {name} puts the old definition back"
            )
        };
        // Applied already: the controller manages it, and only `vm remove`
        // lets it go.
        match still_managed_in(name) {
            Ok(None) => {}
            Ok(Some(copy)) => return by_hand(format!("the controller runs {}", copy.display())),
            Err(why) => return by_hand(why),
        }
        ui.say(&format!("putting {name} back as it was before vm add"));
        if self.backup.is_some() {
            if let Err(why) = define(libvirt, name, self.xml) {
                return by_hand(why);
            }
        }
        if let Err(why) = self.remove_written(ui) {
            return by_hand(why);
        }
        crate::audit::record(context, "vm_add_undone", Some(name), &[]);
        let definition = if self.backup.is_some() {
            " its old definition is back, from its next start;".to_owned()
        } else {
            String::new()
        };
        format!(
            "{error}\n{name} is as it was before vm add:{definition} nothing of RAMpire's is kept for it. rampire vm add {name} tries again"
        )
    }
}

/// The new VM's sizes: suggested from its definition, as given otherwise.
fn chosen_settings(
    inspection: &crate::vm::Inspection,
    options: &AddOptions,
) -> Result<crate::vm::Suggested, String> {
    let mut chosen = crate::vm::suggest(inspection, true)
        .ok_or_else(|| format!("{} has no virtio-mem device", options.vm))?;
    let size = |text: Option<&str>, default: u64| -> Result<u64, String> {
        text.map_or(Ok(default), |text| {
            rampire_core::parse_byte_size(text).map_err(|error| error.to_string())
        })
    };
    chosen.min_ram = size(options.min_ram.as_deref(), chosen.min_ram)?;
    chosen.max_ram = size(options.max_ram.as_deref(), chosen.max_ram)?;
    chosen.free_ram = size(options.free_ram.as_deref(), chosen.free_ram)?;
    if chosen.min_ram > chosen.max_ram {
        return Err("minimum RAM is more than maximum RAM".to_owned());
    }
    if chosen.free_ram >= chosen.max_ram {
        return Err("free RAM must be less than maximum RAM".to_owned());
    }
    Ok(chosen)
}

/// Save `fixed` as `name`'s definition, keeping `xml`, the one before, in
/// [`DOMAIN_BACKUPS`]. Returns the backup.
fn redefine(
    libvirt: &crate::attest::Libvirt,
    name: &str,
    xml: &str,
    fixed: &str,
) -> Result<PathBuf, String> {
    let backups = Path::new(DOMAIN_BACKUPS);
    std::fs::create_dir_all(backups).map_err(|error| error.to_string())?;
    let backup = backups.join(backup_name(name, process::unix_millis()?));
    std::fs::write(&backup, xml).map_err(|error| format!("keep {}: {error}", backup.display()))?;
    define(libvirt, name, fixed)?;
    Ok(backup)
}

/// Save `xml` as `name`'s definition.
fn define(libvirt: &crate::attest::Libvirt, name: &str, xml: &str) -> Result<(), String> {
    let temporary = TempDir::new(name)?;
    let path = temporary.path().join("definition.xml");
    std::fs::write(&path, xml).map_err(|error| error.to_string())?;
    libvirt.virsh(&["define", &path_text(&path)?])?;
    Ok(())
}

/// `name`'s definition before `vm add` first changed it: its oldest backup.
fn original_definition(name: &str) -> Option<String> {
    let oldest = std::fs::read_dir(DOMAIN_BACKUPS)
        .ok()?
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|file| is_backup_of(file, name))
        .min_by_key(|file| {
            file.rsplit_once('-')
                .map_or("", |(_, millis)| millis)
                .trim_end_matches(".xml")
                .parse::<u64>()
                .unwrap_or(u64::MAX)
        })?;
    std::fs::read_to_string(Path::new(DOMAIN_BACKUPS).join(oldest)).ok()
}

/// Wait for the guest service to report in, and say how it is. While it
/// waits it says what it sees every [`WAIT_REPORT`], so a long wait isn't
/// silent; when the wait ends without it, why, as far as the host can tell.
fn guest_reports_in(
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
                    crate::reports::status_line(&status)
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
/// tell, the first cause found: the VM isn't running, it runs an older
/// definition, the service in it isn't running (with its last log lines),
/// or what the controller says.
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
    if runs_behind(libvirt, vm) == Some(true) {
        return format!(
            "{vm} still runs its old definition, without RAMpire's device and channel: it needs a restart (rampire vm restart {vm})"
        );
    }
    let service = match crate::guest::service_state(libvirt, vm) {
        Some(state) if state.starts_with("active") => state,
        Some(state) => {
            return format!(
                "its guest service isn't running: {state}. In {vm}: systemctl status rampire-guest"
            )
        }
        None => {
            return format!(
                "its guest agent doesn't run commands, so RAMpire can't see inside {vm}. In {vm}: systemctl status rampire-guest"
            )
        }
    };
    // The guest is up: what is missing is between it and the controller.
    // The controller says why it can't reach the channel; a guest without
    // one writes control files instead, which the controller reads through
    // the agent, so its error about those files says nothing here.
    match crate::journal::channel_event(vm, Path::new("/")) {
        Some(event) if event.starts_with("unavailable") => {
            return format!(
                "its guest service runs ({service}), but the controller can't reach {vm}'s control channel: {}",
                event.trim_start_matches("unavailable: ")
            )
        }
        Some(_) => {
            return format!(
                "its guest service runs ({service}) and the controller is connected to its control channel, but nothing has come over it yet"
            )
        }
        None => {}
    }
    match crate::inventory::read_status(context, vm) {
        Ok(status) => {
            let problems: Vec<String> = crate::doctor_vm::controller(vm, &status)
                .into_iter()
                .filter(|finding| !finding.ok)
                .map(|finding| finding.detail)
                .collect();
            if problems.is_empty() {
                format!(
                    "its guest service runs ({service}), but the controller has had nothing from it yet and hasn't said why: rampire logs --vm {vm}"
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

/// Whether the running `vm` lacks what its saved definition gives RAMpire,
/// so it needs a cold start; `None` when libvirt can't say.
fn runs_behind(libvirt: &crate::attest::Libvirt, vm: &str) -> Option<bool> {
    let saved = crate::vm::inspect(&libvirt.virsh(&["dumpxml", "--inactive", vm]).ok()?).ok()?;
    let live = crate::vm::inspect(&libvirt.virsh(&["dumpxml", vm]).ok()?).ok()?;
    Some(saved.runs_behind(&live))
}

/// Fail, saying why, when a VM other than `name` would fail `apply`'s
/// checks. Their recorded setups are brought up to date first, as
/// `apply` does.
fn other_vms_pass(
    context: &Context,
    libvirt: &crate::attest::Libvirt,
    ui: &mut dyn Ui,
    name: &str,
) -> Result<(), String> {
    let draft = context.draft();
    for vm in crate::attest::keep_current(&draft, libvirt, ui)? {
        crate::audit::record(context, "vm_recorded", Some(&vm), &[]);
    }
    let mut report = crate::check::run(&draft)?;
    report
        .vms
        .retain(|vm| vm.vm_name != name && vm.problem.is_some());
    if report.vms.is_empty() {
        return Ok(());
    }
    ui.say(crate::check::text(&report, true).trim_end());
    Err(format!(
        "applying checks every VM, so fix these first; nothing was asked or added for {name}"
    ))
}

/// Ask for the kernel release until the answer is one; an empty answer
/// stops.
fn ask_kernel(ui: &mut dyn Ui, name: &str) -> Result<String, String> {
    loop {
        let answer = ui.text(&format!(
            "{name}'s guest agent isn't answering (installing rampire-guest in it brings one). Its kernel release (uname -r in the guest):"
        ))?;
        let answer = answer.trim();
        if answer.is_empty() {
            return Err(format!(
                "a review needs {name}'s kernel release; nothing was added"
            ));
        }
        if is_kernel_release(answer) {
            return Ok(answer.to_owned());
        }
        ui.say(&format!(
            "{answer} is not a kernel release, such as 6.12.0-1-amd64"
        ));
    }
}

/// Whether `text` looks like a kernel release (`6.12.0-1-amd64`), not a
/// stray answer.
fn is_kernel_release(text: &str) -> bool {
    text.starts_with(|c: char| c.is_ascii_digit())
        && text.contains('.')
        && !text.contains(char::is_whitespace)
}

/// The guest's kernel release from its QEMU guest agent, waiting up to
/// `wait` for the agent to answer.
fn agent_kernel(
    ui: &mut dyn Ui,
    libvirt: &crate::attest::Libvirt,
    name: &str,
    wait: std::time::Duration,
) -> Option<String> {
    let deadline = std::time::Instant::now() + wait;
    let cancel = ui.cancel();
    let mut said = false;
    loop {
        let kernel = libvirt
            .virsh(&[
                "qemu-agent-command",
                name,
                r#"{"execute":"guest-get-osinfo"}"#,
            ])
            .ok()
            .as_deref()
            .and_then(crate::vm::kernel_release_from_osinfo);
        if kernel.is_some() || std::time::Instant::now() >= deadline || cancel.is_set() {
            return kernel;
        }
        if !said {
            ui.say(&format!("waiting for {name}'s guest agent"));
            said = true;
        }
        cancel.sleep(std::time::Duration::from_secs(3));
    }
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

/// A new review's yes-or-no answers and the guest's kernel release, each
/// with where it came from.
type NewReview = (
    Vec<(&'static str, bool, AnswerSource)>,
    (String, AnswerSource),
);

/// The review's yes-or-no answers: read from the definition where it shows
/// them, asked otherwise; the one about the guest's programs was agreed to. One that isn't true means the VM can't be managed.
fn ask_review(
    ui: &mut dyn Ui,
    inspection: &crate::vm::Inspection,
    name: &str,
) -> Result<Vec<(&'static str, bool, AnswerSource)>, String> {
    use crate::vm;
    let mut answers = Vec::new();
    for detected in vm::review_answers(inspection, vm::host_has_rdma()) {
        // Answered when the person agreed to add the VM.
        if detected.question == "workload_reviewed" {
            answers.push((detected.question, true, AnswerSource::Confirmed));
            continue;
        }
        let (value, source) = match detected.answer {
            Some(answer) => answer,
            None => (
                ui.review(vm::question_text(detected.question))?,
                AnswerSource::Confirmed,
            ),
        };
        if !value {
            return Err(format!(
                "{name} can't be managed: {}; nothing was added",
                vm::unsafe_text(detected.question)
            ));
        }
        answers.push((detected.question, value, source));
    }
    Ok(answers)
}

/// The guest's kernel release: given, else read from its QEMU guest agent,
/// waiting up to `agent_wait`, else asked.
fn kernel_release(
    ui: &mut dyn Ui,
    libvirt: &crate::attest::Libvirt,
    options: &AddOptions,
    agent_wait: std::time::Duration,
) -> Result<(String, AnswerSource), String> {
    let name = options.vm.as_str();
    match options.kernel.as_deref() {
        Some(kernel) if is_kernel_release(kernel) => {
            Ok((kernel.to_owned(), AnswerSource::Confirmed))
        }
        Some(kernel) => Err(format!(
            "--kernel {kernel} is not a kernel release, such as 6.12.0-1-amd64"
        )),
        None => match agent_kernel(ui, libvirt, name, agent_wait) {
            Some(kernel) => {
                ui.say(&format!(
                    "{name} runs Linux {kernel} (read from its guest agent)"
                ));
                Ok((kernel, AnswerSource::Detected))
            }
            None => Ok((ask_kernel(ui, name)?, AnswerSource::Confirmed)),
        },
    }
}

/// Write a new VM's settings, with its review profile: the one named, or
/// one written from `review`.
fn write_vm_settings(
    ui: &mut dyn Ui,
    directory: &Path,
    inspection: &crate::vm::Inspection,
    alias: &str,
    options: &AddOptions,
    chosen: &crate::vm::Suggested,
    review: Option<NewReview>,
) -> Result<(), String> {
    use crate::vm;
    use rampire_core::settings;
    let name = options.vm.as_str();
    let settings_path = settings_path(directory, name)?;
    let settings_path = settings_path.as_path();
    let review = match (options.review.as_deref(), review) {
        (Some(profile), _) => {
            let path = directory
                .join(crate::applied::REVIEW_DIRECTORY)
                .join(format!("{profile}.toml"));
            rampire_host::attestation::read_review(&path)?;
            profile.to_owned()
        }
        (None, None) => return Err(format!("{name} has no review to write")),
        (None, Some((answers, (kernel, source)))) => {
            let text = vm::review_toml(
                name,
                &answers,
                inspection.max_memory_slots.unwrap_or(1),
                (&format!("linux {kernel}"), source),
            );
            // Named for the VM's files: `^web01` for Web01.
            let profile = settings::vm_file_stem(name);
            let path = directory
                .join(crate::applied::REVIEW_DIRECTORY)
                .join(format!("{profile}.toml"));
            write_new(&path, &text)?;
            rampire_host::attestation::read_review(&path)?;
            ui.say(&format!("wrote the review {}", path.display()));
            profile
        }
    };

    write_new(
        settings_path,
        &vm::vm_toml(alias, options.priority, &review, chosen),
    )?;
    if let Err(error) = settings::load_vm(settings_path) {
        return Err(match std::fs::remove_file(settings_path) {
            Ok(()) => format!("{error}; nothing was added"),
            Err(removal) => format!(
                "{error}; remove {} by hand: {removal}",
                settings_path.display()
            ),
        });
    }
    ui.say(&format!("wrote {}", settings_path.display()));
    Ok(())
}

/// `rampire vm remove`: stop managing a VM, put its definition back to
/// plain RAM, and remove everything RAMpire kept for it. Its settings
/// are put back if the controller can't be moved off it, so a failed
/// removal leaves it managed as it was.
pub fn vm_remove(context: &Context, ui: &mut dyn Ui, name: &str) -> Result<Outcome, String> {
    use rampire_core::settings;
    let directory = context.draft();
    let path = settings_path(&directory, name)?;
    if !path.exists() {
        return clear_definition(&directory, context, ui, name);
    }
    let vm = settings::load_vm(&path)?;
    if !ui.confirm(&format!(
        "Remove RAMpire from {name}? It stops managing it, and from {name}'s next start its \
         definition has plain RAM again, its boot RAM, for Cockpit or virsh to change."
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let libvirt = context.libvirt()?;
    // The controller lets go of the VM first; then what it kept goes.
    without_settings(&path, name, || super::apply::apply_agreed(context, ui))?;
    crate::audit::record(context, "vm_remove", Some(name), &[]);
    // From here RAMpire no longer manages it: a failure says what is left.
    let unmanaged = |error: String| {
        format!(
            "{error}\n{name} is no longer managed; the VM list shows what is left of RAMpire in it, and rampire vm remove {name} tries again"
        )
    };
    if libvirt.state(name).is_ok() {
        let xml = libvirt
            .virsh(&["dumpxml", "--inactive", name])
            .map_err(unmanaged)?;
        let plain =
            crate::vm::plain_xml(&xml, original_definition(name).as_deref()).map_err(unmanaged)?;
        if plain != xml {
            define(&libvirt, name, &plain).map_err(unmanaged)?;
            let ram = crate::vm::inspect(&plain).map_err(unmanaged)?.memory_bytes;
            ui.say(&format!(
                "{name}'s definition has plain RAM again, {}, from its next start",
                format_byte_size(ram)
            ));
        }
    }
    if let Some(copy) = still_managed_in(name).map_err(unmanaged)? {
        ui.say(&format!(
            "{name} is still managed by {}, so what RAMpire keeps for it stays",
            copy.display()
        ));
        return Ok(Outcome::Partly(format!(
            "{name} is out of the configuration, but the controller still runs {}, so its recorded setup and request state stay: rampire doctor says why the controller hasn't moved on",
            copy.display()
        )));
    }
    forget_vm(ui, &directory, name, vm.review.as_deref()).map_err(unmanaged)?;
    ui.say(&format!("{name} is removed"));
    // While it runs, all its RAM is its own again.
    super::controller::offer_pool(context, ui);
    Ok(Outcome::Done)
}

/// Take a VM's settings file at `path` out of the draft and run `then`;
/// when that fails, put the file back as it was, so the VM stays managed.
fn without_settings<T>(
    path: &Path,
    name: &str,
    then: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    let kept = std::fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    std::fs::remove_file(path).map_err(|error| format!("remove {}: {error}", path.display()))?;
    then().map_err(|error| match std::fs::write(path, &kept) {
        Ok(()) => format!(
            "{error}\n{name} is still managed: its settings are back in {}, and nothing else was changed",
            path.display()
        ),
        Err(write) => format!(
            "{error}\n{name}'s settings were taken out of the draft and couldn't be put back ({write}): rampire vm remove {name} tries again"
        ),
    })
}

/// `vm remove` for a VM RAMpire no longer manages whose definition still
/// has its parts: the definition goes back to plain RAM, and what RAMpire
/// kept for it goes.
fn clear_definition(
    directory: &Path,
    context: &Context,
    ui: &mut dyn Ui,
    name: &str,
) -> Result<Outcome, String> {
    let libvirt = context.libvirt()?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
    let inspection = crate::vm::inspect(&xml)?;
    if !inspection.control_channel && inspection.device().is_none() {
        return Err(format!(
            "RAMpire doesn't manage {name}, and its definition has nothing of RAMpire's"
        ));
    }
    if !ui.confirm(&format!(
        "Remove RAMpire's parts from {name}'s definition? From its next start it has plain RAM \
         again, its boot RAM, for Cockpit or virsh to change."
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let plain = crate::vm::plain_xml(&xml, original_definition(name).as_deref())?;
    define(&libvirt, name, &plain)?;
    ui.say(&format!(
        "{name}'s definition has plain RAM again, {}, from its next start",
        format_byte_size(crate::vm::inspect(&plain)?.memory_bytes)
    ));
    crate::audit::record(context, "vm_remove", Some(name), &[]);
    forget_vm(
        ui,
        directory,
        name,
        Some(&rampire_core::settings::vm_file_stem(name)),
    )?;
    Ok(Outcome::Done)
}

/// Remove what RAMpire keeps for a VM it no longer manages: its recorded
/// setup, request state and definition backups, and its
/// review profile when `vm add` wrote it for this VM (named after it) and
/// no other VM's settings name it.
fn forget_vm(
    ui: &mut dyn Ui,
    directory: &Path,
    name: &str,
    review: Option<&str>,
) -> Result<(), String> {
    for path in forgotten_files(directory, name, review)? {
        match std::fs::remove_file(&path) {
            Ok(()) => ui.say(&format!("removed {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

/// The files [`forget_vm`] removes, those that exist or not.
pub fn forgotten_files(
    directory: &Path,
    name: &str,
    review: Option<&str>,
) -> Result<Vec<PathBuf>, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    let stem = settings::vm_file_stem(name);
    let mut gone = vec![rampire_host::config::attestation_path(directory, name)];
    let state = Path::new(rampire_host::config::DEFAULT_STATE_DIRECTORY);
    for suffix in ["requests.json", "requests.json.lock"] {
        gone.push(state.join(format!("{stem}-{suffix}")));
    }
    if let Ok(entries) = std::fs::read_dir(DOMAIN_BACKUPS) {
        for entry in entries.flatten() {
            if is_backup_of(&entry.file_name().to_string_lossy(), name) {
                gone.push(entry.path());
            }
        }
    }
    if review == Some(stem.as_str()) {
        let own = std::ffi::OsString::from(format!("{stem}.toml"));
        let named_elsewhere = std::fs::read_dir(directory.join(VMS_DIRECTORY))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name() != own)
            .filter_map(|entry| settings::load_vm(&entry.path()).ok())
            .any(|other| other.review.as_deref() == Some(stem.as_str()));
        if !named_elsewhere {
            gone.push(
                directory
                    .join(crate::applied::REVIEW_DIRECTORY)
                    .join(format!("{stem}.toml")),
            );
        }
    }
    Ok(gone)
}

/// The file in [`DOMAIN_BACKUPS`] that keeps `name`'s definition as it was
/// before `vm add` changed it at `unix_millis`.
fn backup_name(name: &str, unix_millis: u64) -> String {
    format!(
        "{}-{unix_millis}.xml",
        rampire_core::settings::vm_file_stem(name)
    )
}

/// Whether `file` is one of `name`'s backups: not another VM's whose name
/// starts with it (`web-2-...` for `web`).
fn is_backup_of(file: &str, name: &str) -> bool {
    file.strip_prefix(rampire_core::settings::vm_file_stem(name).as_str())
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(".xml"))
        .is_some_and(|millis| !millis.is_empty() && millis.bytes().all(|b| b.is_ascii_digit()))
}

/// The applied copy, current or running, that still manages `name`. Its
/// checkpoint and lock belong to the controller until neither does.
fn still_managed_in(name: &str) -> Result<Option<PathBuf>, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    let filename = settings::vm_filename(name)?;
    let running = running_copy();
    let current = crate::applied::current(applied_root())?;
    Ok([current, running]
        .into_iter()
        .flatten()
        .find(|copy| copy.join(VMS_DIRECTORY).join(&filename).exists()))
}

/// `rampire vm restart`: shut the VM down and start it again, so its saved
/// definition takes effect, then apply, which records its new setup.
pub fn vm_restart(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<Outcome, String> {
    crate::attest::vm_settings(&context.draft(), vm)?;
    if !ui.confirm(&format!(
        "Shut {vm} down and start it again, so its saved definition takes effect?"
    ))? {
        ui.say("nothing changed");
        return Ok(Outcome::declined());
    }
    let libvirt = context.libvirt()?;
    crate::attest::cold_restart(vm, &libvirt, std::time::Duration::from_secs(300))?;
    ui.say(&format!("{vm} restarted"));
    crate::audit::record(context, "vm_restart", Some(vm), &[]);
    Ok(match apply(context, ui)? {
        Outcome::Declined(_) => Outcome::Partly(format!(
            "{vm} restarted, but its new setup isn't applied: rampire apply records it"
        )),
        _ => Outcome::Done,
    })
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
    if !upgrade_guest(ui, &libvirt, vm, login, true)? {
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
/// can't be logged in to by itself, its login is asked first. `false` when
/// the person answered No to shutting it down: nothing was changed.
fn upgrade_guest(
    ui: &mut dyn Ui,
    libvirt: &crate::attest::Libvirt,
    vm: &str,
    login: Option<&str>,
    may_shut_down: bool,
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
    // anything that shuts the VM down.
    if let (Err(why), None) = (&logged_in, login) {
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
fn upgrade_if_behind(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<(), String> {
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
    match upgrade_guest(ui, &libvirt, vm, None, false) {
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

/// `rampire set`: change settings in the draft, then apply unless
/// `no_apply`. A VM whose guest runs another build than this host's has
/// its guest upgraded as well. [`Outcome::Partly`] when the change is
/// saved but not applied, other than with `no_apply`, which asks for
/// that.
pub fn set(
    context: &Context,
    ui: &mut dyn Ui,
    target: &str,
    changes: &[crate::set::Assignment],
    no_apply: bool,
) -> Result<Outcome, String> {
    let path = crate::set::set(&context.draft(), target, changes)?;
    ui.say(&format!("changed {}", path.display()));
    let described: Vec<String> = changes
        .iter()
        .map(|change| {
            let value = change.value.to_string();
            format!("{}={}", change.key, value.trim().trim_matches('"'))
        })
        .collect();
    crate::audit::record(
        context,
        "set",
        (target != crate::set::GLOBAL).then_some(target),
        &[("changes", &described.join(","))],
    );
    let saved = |error: String| {
        format!(
            "{error}\nThe change is saved in {} but not applied: fix what is said above and run rampire apply, or set the old value back",
            path.display()
        )
    };
    if target != crate::set::GLOBAL {
        grow_for_max_ram(context, ui, target).map_err(saved)?;
        upgrade_if_behind(context, ui, target).map_err(saved)?;
    }
    let not_applied = format!(
        "saved in {}, not applied: rampire apply applies it",
        path.display()
    );
    if no_apply {
        ui.say("not applied: run rampire apply when ready");
        return Ok(Outcome::Done);
    }
    Ok(match apply(context, ui).map_err(saved)? {
        Outcome::Declined(_) => Outcome::Partly(not_applied),
        outcome => outcome,
    })
}

/// When a VM's `max_ram` is more than its virtio-mem device holds, grow the
/// device in its definition. That takes a restart, so ask whether now; else
/// it takes effect at the VM's next start. Within the device, a change needs
/// nothing here: the controller takes it at once.
fn grow_for_max_ram(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<(), String> {
    use std::time::Duration;
    let settings = crate::attest::vm_settings(&context.draft(), vm)?;
    let libvirt = context.libvirt()?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", vm])?;
    let inspection = crate::vm::inspect(&xml)?;
    let Some(device) = inspection.device() else {
        return Ok(());
    };
    let boot = inspection.memory_bytes.saturating_sub(device.size_bytes);
    if settings.max_ram <= boot + device.size_bytes {
        return Ok(());
    }
    let block = device.block_bytes.max(2 * 1024 * 1024);
    let size = (settings.max_ram - boot).div_ceil(block) * block;
    let grown = crate::vm::grow_device(&xml, size)?;
    let backup = redefine(&libvirt, vm, &xml, &grown)?;
    crate::audit::record(
        context,
        "fix_xml",
        Some(vm),
        &[("backup", &backup.display().to_string())],
    );
    ui.say(&format!(
        "{vm}'s memory device grows from {} to {} for maximum RAM {}; the old definition is in {}",
        format_byte_size(device.size_bytes),
        format_byte_size(size),
        format_byte_size(settings.max_ram),
        backup.display()
    ));
    if libvirt.state(vm)? != "running" {
        ui.say(&format!("it takes effect when {vm} starts"));
        return Ok(());
    }
    if ui.confirm(&format!(
        "The bigger device takes a restart. Restart {vm} now? (No: it grows at its next start)"
    ))? {
        crate::attest::cold_restart(vm, &libvirt, Duration::from_secs(300))?;
        ui.say(&format!("{vm} restarted"));
    } else {
        ui.say(&format!(
            "{vm} grows to {} at its next start; until then it stays within {}",
            format_byte_size(settings.max_ram),
            format_byte_size(boot + device.size_bytes)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::Transcript;

    #[test]
    fn settings_for_a_vm_with_settings_are_told_apart() {
        let mut options = AddOptions::new("web");
        assert!(!options.has_settings());
        options.max_ram = Some("4GB".to_owned());
        assert!(options.has_settings());
    }

    fn context(draft: &Path) -> Context {
        Context {
            config: Some(draft.to_path_buf()),
            connect: Some("test:///default".to_owned()),
            build: String::new(),
            via: "command",
        }
    }

    #[test]
    fn a_vms_backups_are_its_own() {
        let backup = backup_name("web", 1_700_000_000_000);
        assert!(is_backup_of(&backup, "web"));
        assert!(!is_backup_of(
            &backup_name("web-2", 1_700_000_000_000),
            "web"
        ));
        assert!(!is_backup_of("web-notes.xml", "web"));
        assert!(!is_backup_of("web-.xml", "web"));
        assert!(!is_backup_of(&backup, "we"));
        // A capital letter is written as in the VM's other files.
        let capital = backup_name("Web", 1_700_000_000_000);
        assert_eq!(capital, "^web-1700000000000.xml");
        assert!(is_backup_of(&capital, "Web"));
        assert!(!is_backup_of(&capital, "web"));
        assert!(!is_backup_of(&backup, "Web"));
    }

    #[test]
    fn removing_a_vm_that_isnt_managed_reads_its_definition_before_asking() {
        // Unmanaged, it is about the definition: here libvirt can't give
        // one, which fails before anything is asked.
        let draft = TempDir::new("test-unmanaged").unwrap();
        let mut ui = Transcript::default();
        assert!(vm_remove(&context(draft.path()), &mut ui, "web").is_err());
        assert!(ui.said.is_empty());
    }

    #[test]
    fn a_removal_the_controller_cant_take_puts_the_settings_back() {
        let draft = TempDir::new("test-remove-back").unwrap();
        let path = draft.path().join("web.toml");
        std::fs::write(&path, "priority = 100\n").unwrap();
        let error = without_settings(&path, "web", || -> Result<(), String> {
            assert!(!path.exists(), "out of the draft while applying");
            Err("the draft did not pass its checks".to_owned())
        })
        .unwrap_err();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "priority = 100\n");
        assert!(error.contains("still managed"), "{error}");
        // When it goes through, the settings stay out.
        without_settings(&path, "web", || Ok(())).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn declining_changes_nothing_and_says_so() {
        // Each flow asks before it changes anything: a No ends it, and the
        // outcome says so, not that it was done.
        let draft = TempDir::new("test-decline").unwrap();
        let vms = draft.path().join(rampire_core::settings::VMS_DIRECTORY);
        std::fs::create_dir_all(&vms).unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/config/linux-guest.toml");
        std::fs::copy(fixture, vms.join("web.toml")).unwrap();
        let mut ui = Transcript::answering(&["n", "n"]);
        let outcome = vm_remove(&context(draft.path()), &mut ui, "web").unwrap();
        assert_eq!(outcome, Outcome::declined());
        assert!(vms.join("web.toml").exists(), "still managed");
        assert_eq!(
            vm_restart(&context(draft.path()), &mut ui, "web").unwrap(),
            Outcome::declined()
        );
    }

    #[test]
    fn a_failed_add_takes_out_only_what_it_wrote() {
        let draft = TempDir::new("test-undo").unwrap();
        let written = draft.path().join("web.toml");
        let review = draft.path().join("review-web.toml");
        let mut undo = Undo {
            name: "web",
            xml: "<domain/>",
            backup: None,
            settings: Some(written.clone()),
            review: Some(review.clone()),
            recorded: None,
        };
        // It stopped before writing anything: nothing to undo.
        assert!(!undo.has_work());
        std::fs::write(&written, "priority = 100\n").unwrap();
        assert!(undo.has_work());
        // A review that was there before is not this run's.
        let kept = draft.path().join("kept.toml");
        std::fs::write(&kept, "kept").unwrap();
        undo.review = None;
        let mut ui = Transcript::default();
        undo.remove_written(&mut ui).unwrap();
        assert!(!written.exists());
        assert!(kept.exists());
        assert!(!review.exists());
        // Changing the definition is work even with no files written.
        undo.backup = Some(draft.path().join("web-1.xml"));
        assert!(undo.has_work());
    }

    #[test]
    fn only_a_managed_vm_is_upgraded() {
        let draft = TempDir::new("test-upgrade").unwrap();
        let mut ui = Transcript::default();
        assert!(vm_upgrade(&context(draft.path()), &mut ui, "web", None).is_err());
        assert!(ui.said.is_empty());
    }
}
