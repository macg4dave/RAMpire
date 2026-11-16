//! `rampire vm add`: bringing a VM under management, and taking out what
//! a failed add wrote.

use super::*;

/// `vm add`'s options beside the VM's name.
#[derive(Debug, Clone)]
pub struct AddOptions {
    pub vm: String,
    pub review: Option<String>,
    pub kernel: Option<String>,
    pub priority: u32,
    pub min_ram: Option<String>,
    pub max_ram: Option<String>,
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
    // RAMpire's parts: the device, the channels, the cell they need.
    let with_parts = vm::fix_xml(&xml, &inspection)?;
    let parts = match &with_parts {
        Some(fixed) => vm::inspect(fixed)?,
        None => inspection.clone(),
    };
    let problems = parts.problems();
    if !problems.is_empty() {
        for (problem, fix) in &problems {
            ui.say(&format!("problem: {problem}"));
            ui.say(&format!("  fix: {fix}"));
        }
        return Err(format!("{name} can't be managed yet; nothing was changed"));
    }
    // Without problems there is one device, and it has an alias.
    let alias = parts
        .device()
        .and_then(|device| device.alias.clone())
        .ok_or_else(|| format!("{name} has no virtio-mem device with an alias"))?;
    // The sizes first: they need only the definition, and the steps after
    // this read the disk and check every other VM.
    let settings = if kept {
        None
    } else {
        Some(chosen_settings(&parts, options)?)
    };
    let (min_ram, max_ram) = match &settings {
        Some(chosen) => (chosen.min_ram, chosen.max_ram),
        None => {
            let kept = rampire_core::settings::load_vm(&settings_path)?;
            (kept.min_ram, kept.max_ram)
        }
    };
    // Then the definition is fitted to them: the VM boots with its
    // minimum, and its device holds the rest up to its maximum.
    let with_parts_text = with_parts.as_deref().unwrap_or(&xml);
    let fitted = vm::fit_memory(with_parts_text, min_ram, max_ram)
        .map_err(|error| format!("{name}: {error}; nothing was changed"))?;
    let resized = fitted != with_parts_text;
    let fixed = (fitted != xml).then_some(fitted);
    let after = match &fixed {
        Some(fixed) => vm::inspect(fixed)?,
        None => inspection.clone(),
    };

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
    let mut changes = inspection.fixable();
    if resized {
        changes.push(format!(
            "boot with {}, its minimum; its memory device holds the rest, up to {}",
            format_byte_size(min_ram),
            format_byte_size(max_ram)
        ));
    }
    // Every change but saving the requested size takes effect when the VM
    // next starts; a VM fixed by an earlier run may still run the old one.
    let mut restart = running && (inspection.fixes_take_a_restart() || resized);
    if running && !restart {
        restart = after.runs_behind(&vm::inspect(&libvirt.virsh(&["dumpxml", name])?)?);
    }

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
                "priority {}, minimum RAM {}, maximum RAM {}",
                options.priority,
                format_byte_size(chosen.min_ram),
                format_byte_size(chosen.max_ram),
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
        crate::flows::apply::apply_agreed(context, ui)?;
        // A VM still on its old definition can't be resized, and its guest
        // service has no channel to report on: it needs a cold start.
        if crate::vm::runs_behind(&libvirt, name) == Some(true) {
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
    if chosen.min_ram >= chosen.max_ram {
        return Err(format!(
            "maximum RAM {} must be more than minimum RAM {}; nothing was changed",
            format_byte_size(chosen.max_ram),
            format_byte_size(chosen.min_ram)
        ));
    }
    Ok(chosen)
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
}
