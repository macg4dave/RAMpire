//! `vm add`, `vm remove` and `vm restart`: bringing a VM under management
//! (fixing its definition, installing its guest package, writing its review
//! and settings), letting it go with everything RAMpire kept for it, and
//! restarting it onto its saved definition.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::apply::{apply, running_copy};
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
/// for the guest to report in. Returns whether the applied configuration
/// changed.
pub fn vm_add(context: &Context, ui: &mut dyn Ui, options: &AddOptions) -> Result<bool, String> {
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

    let running = libvirt.state(name)? == "running";
    let changes = inspection.fixable();
    // Every change but saving the requested size takes effect when the VM
    // next starts; a VM fixed by an earlier run may still run the old one.
    let mut restart = running && changes.iter().any(|change| !change.starts_with("save"));
    if running && !restart {
        restart = after.runs_behind(&vm::inspect(&libvirt.virsh(&["dumpxml", name])?)?);
    }
    let settings = if kept {
        None
    } else {
        Some(chosen_settings(&after, options)?)
    };

    // What will be done, agreed to once.
    let mut plan = Vec::new();
    match &settings {
        Some(chosen) => plan.push(format!(
            "settings: priority {}, minimum RAM {}, maximum RAM {}, free RAM {}",
            options.priority,
            format_byte_size(chosen.min_ram),
            format_byte_size(chosen.max_ram),
            format_byte_size(chosen.free_ram)
        )),
        None => plan.push(format!(
            "the settings an earlier vm add wrote, in {}",
            settings_path.display()
        )),
    }
    for change in &changes {
        plan.push(format!("its definition: {change}"));
    }
    if !options.no_guest_install {
        plan.push(
            "install or upgrade rampire-guest in it, the build this host runs: logged in to it, \
             or with it shut down for a moment when it can't be logged in to"
                .to_owned(),
        );
    }
    if !running {
        plan.push("start it".to_owned());
    } else if restart {
        plan.push("shut it down and start it, so its new definition takes effect".to_owned());
    }
    let mut shown = format!("Adding {name} to RAMpire:\n");
    for line in &plan {
        shown.push_str(&format!("  - {line}\n"));
    }
    ui.show(&shown);
    let question = format!("RAMpire then grows and shrinks {name}'s memory while it runs. Add it?");
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
        return Ok(false);
    }
    let answers = if new_review {
        ask_review(ui, &after, name)?
    } else {
        Vec::new()
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
    }
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
        libvirt.virsh(&["start", name])?;
        ui.say(&format!("{name} started"));
    }
    if !options.no_guest_install && installed.is_none() {
        let logged_in = crate::guest::Guest::reach(
            &libvirt,
            name,
            options.guest_login.as_deref(),
            Duration::from_secs(180),
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
    let changed = super::apply::apply_agreed(context, ui)?;
    // An install at boot may boot the VM once more.
    guest_reports_in(context, ui, name, Duration::from_secs(300));
    Ok(changed)
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
            file.trim_start_matches(name)
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse::<u64>()
                .unwrap_or(u64::MAX)
        })?;
    std::fs::read_to_string(Path::new(DOMAIN_BACKUPS).join(oldest)).ok()
}

/// Wait for the guest service to report in, and say how it is.
fn guest_reports_in(context: &Context, ui: &mut dyn Ui, vm: &str, limit: std::time::Duration) {
    ui.say(&format!("waiting for {vm}'s guest service to report in"));
    let started = std::time::Instant::now();
    while started.elapsed() < limit {
        if let Ok(status) = crate::inventory::read_status(context, vm) {
            if status.guest_input.observed_unix_millis.is_some() {
                ui.say(&format!(
                    "{vm} is added: {}",
                    crate::reports::status_line(&status)
                ));
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
    ui.say(&format!(
        "{vm} is added, but its guest service hasn't reported in after {} s: rampire doctor {vm} says why",
        limit.as_secs()
    ));
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
        if kernel.is_some() || std::time::Instant::now() >= deadline {
            return kernel;
        }
        if !said {
            ui.say(&format!("waiting for {name}'s guest agent"));
            said = true;
        }
        std::thread::sleep(std::time::Duration::from_secs(3));
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
            let path = directory
                .join(crate::applied::REVIEW_DIRECTORY)
                .join(format!("{name}.toml"));
            write_new(&path, &text)?;
            rampire_host::attestation::read_review(&path)?;
            ui.say(&format!("wrote the review {}", path.display()));
            name.to_owned()
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
/// plain RAM, and remove everything RAMpire kept for it.
pub fn vm_remove(context: &Context, ui: &mut dyn Ui, name: &str) -> Result<(), String> {
    use rampire_core::settings;
    let directory = context.draft();
    let path = settings_path(&directory, name)?;
    let vm = settings::load_vm(&path)?;
    if !ui.confirm(&format!(
        "Remove RAMpire from {name}? It stops managing it, and from {name}'s next start its \
         definition has plain RAM again, its boot RAM, for Cockpit or virsh to change."
    ))? {
        ui.say("nothing changed");
        return Ok(());
    }
    let libvirt = context.libvirt()?;
    std::fs::remove_file(&path).map_err(|error| format!("remove {}: {error}", path.display()))?;
    crate::audit::record(context, "vm_remove", Some(name), &[]);
    // The controller lets go of the VM first; then what it kept goes.
    super::apply::apply_agreed(context, ui)?;
    if libvirt.state(name).is_ok() {
        let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
        let plain = crate::vm::plain_xml(&xml, original_definition(name).as_deref())?;
        if plain != xml {
            define(&libvirt, name, &plain)?;
            let ram = crate::vm::inspect(&plain)?.memory_bytes;
            ui.say(&format!(
                "{name}'s definition has plain RAM again, {}, from its next start",
                format_byte_size(ram)
            ));
        }
    }
    if let Some(copy) = still_managed_in(name)? {
        ui.say(&format!(
            "{name} is still managed by {}, so what RAMpire keeps for it stays",
            copy.display()
        ));
        return Ok(());
    }
    forget_vm(ui, &directory, name, vm.review.as_deref())?;
    ui.say(&format!("{name} is removed"));
    Ok(())
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
    let mut gone = vec![rampire_host::config::attestation_path(directory, name)];
    let state = Path::new(rampire_host::config::DEFAULT_STATE_DIRECTORY);
    for suffix in ["requests.json", "requests.json.lock"] {
        gone.push(state.join(format!("{name}-{suffix}")));
    }
    if let Ok(entries) = std::fs::read_dir(DOMAIN_BACKUPS) {
        for entry in entries.flatten() {
            if is_backup_of(&entry.file_name().to_string_lossy(), name) {
                gone.push(entry.path());
            }
        }
    }
    if review == Some(name) {
        let named_elsewhere = std::fs::read_dir(directory.join(VMS_DIRECTORY))
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name() != std::ffi::OsString::from(format!("{name}.toml")))
            .filter_map(|entry| settings::load_vm(&entry.path()).ok())
            .any(|other| other.review.as_deref() == Some(name));
        if !named_elsewhere {
            gone.push(
                directory
                    .join(crate::applied::REVIEW_DIRECTORY)
                    .join(format!("{name}.toml")),
            );
        }
    }
    Ok(gone)
}

/// The file in [`DOMAIN_BACKUPS`] that keeps `name`'s definition as it was
/// before `vm add` changed it at `unix_millis`.
fn backup_name(name: &str, unix_millis: u64) -> String {
    format!("{name}-{unix_millis}.xml")
}

/// Whether `file` is one of `name`'s backups: not another VM's whose name
/// starts with it (`web-2-...` for `web`).
fn is_backup_of(file: &str, name: &str) -> bool {
    file.strip_prefix(name)
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
pub fn vm_restart(context: &Context, ui: &mut dyn Ui, vm: &str) -> Result<(), String> {
    crate::attest::vm_settings(&context.draft(), vm)?;
    if !ui.confirm(&format!(
        "Shut {vm} down and start it again, so its saved definition takes effect?"
    ))? {
        ui.say("nothing changed");
        return Ok(());
    }
    let libvirt = context.libvirt()?;
    crate::attest::cold_restart(vm, &libvirt, std::time::Duration::from_secs(300))?;
    ui.say(&format!("{vm} restarted"));
    crate::audit::record(context, "vm_restart", Some(vm), &[]);
    apply(context, ui)?;
    Ok(())
}

/// `rampire set`: change settings in the draft, then apply unless
/// `no_apply`.
pub fn set(
    context: &Context,
    ui: &mut dyn Ui,
    target: &str,
    changes: &[crate::set::Assignment],
    no_apply: bool,
) -> Result<bool, String> {
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
    if no_apply {
        ui.say("not applied: run rampire apply when ready");
        return Ok(false);
    }
    apply(context, ui)
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
    }

    #[test]
    fn removing_a_vm_that_isnt_managed_says_so_before_asking() {
        let draft = TempDir::new("test-unmanaged").unwrap();
        let mut ui = Transcript::default();
        let error = vm_remove(&context(draft.path()), &mut ui, "web").unwrap_err();
        assert!(error.contains("web.toml"), "{error}");
        assert!(ui.said.is_empty());
    }
}
