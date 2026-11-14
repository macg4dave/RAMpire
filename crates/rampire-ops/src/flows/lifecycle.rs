//! `vm add`, `vm remove` and `vm attest`: bringing a VM under management
//! (fixing its definition, writing its review and settings, restoring
//! parked settings), parking or forgetting it, and attesting it again.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::apply::{apply, running_copy};
use crate::context::{applied_root, Context, TempDir, DOMAIN_BACKUPS, PARKED_DIRECTORY};
use crate::process;
use crate::ui::Ui;

/// `vm add`'s options beside the VM's name.
#[derive(Debug, Clone, Default)]
pub struct AddOptions {
    pub vm: String,
    pub fix_xml: bool,
    pub review: Option<String>,
    pub kernel: Option<String>,
    pub priority: u32,
    pub min_ram: Option<String>,
    pub max_ram: Option<String>,
    pub free_ram: Option<String>,
}

impl AddOptions {
    /// A person's `vm add VM --fix-xml`, with every default.
    pub fn fixing(vm: &str) -> Self {
        Self {
            vm: vm.to_owned(),
            fix_xml: true,
            priority: 100,
            ..Self::default()
        }
    }

    fn has_settings(&self) -> bool {
        self.review.is_some()
            || self.kernel.is_some()
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

/// Where a VM's settings are in the draft: managed, or parked.
fn settings_paths(directory: &Path, name: &str) -> Result<(PathBuf, PathBuf), String> {
    let filename = rampire_core::settings::vm_filename(name)?;
    Ok((
        directory
            .join(rampire_core::settings::VMS_DIRECTORY)
            .join(&filename),
        directory.join(PARKED_DIRECTORY).join(&filename),
    ))
}

/// `rampire vm add`. Returns whether the applied configuration changed.
pub fn vm_add(context: &Context, ui: &mut dyn Ui, options: &AddOptions) -> Result<bool, String> {
    use crate::vm;
    use rampire_core::settings::VMS_DIRECTORY;
    let name = options.vm.as_str();
    let directory = context.draft();
    let filename = rampire_core::settings::vm_filename(name)?;
    let (settings_path, parked) = settings_paths(&directory, name)?;
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
        if options.has_settings() {
            return Err(format!(
                "{name} has settings from an earlier vm add in {}: run vm add {name} without settings to finish it, or change them with rampire set",
                settings_path.display()
            ));
        }
        ui.say(&format!(
            "{name} has settings from an earlier vm add that didn't finish ({}); finishing it",
            settings_path.display()
        ));
    }
    let libvirt = context.libvirt()?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
    let mut inspection = vm::inspect(&xml)?;
    let mut restart_needed = false;

    let fixable = inspection.fixable();
    if !fixable.is_empty() {
        for change in &fixable {
            ui.say(&format!("to fix: {change}"));
        }
        if !options.fix_xml {
            ui.say(&format!(
                "rampire vm add {name} --fix-xml makes these changes"
            ));
        } else if let Some(fixed) = vm::fix_xml(&xml, &inspection)? {
            let temporary = TempDir::new(name)?;
            let (before, after) = (
                temporary.path().join("before.xml"),
                temporary.path().join("after.xml"),
            );
            std::fs::write(&before, &xml).map_err(|error| error.to_string())?;
            std::fs::write(&after, &fixed).map_err(|error| error.to_string())?;
            let shown = std::process::Command::new("diff")
                .args(["-u", "--label", "saved definition", "--label", "fixed"])
                .arg(&before)
                .arg(&after)
                .output()
                .map_err(|error| format!("run diff: {error}"))?;
            ui.show(&String::from_utf8_lossy(&shown.stdout));
            if !ui.confirm(&format!("Change {name}'s saved definition like this?"))? {
                ui.say("nothing changed");
                return Ok(false);
            }
            let backups = Path::new(DOMAIN_BACKUPS);
            std::fs::create_dir_all(backups).map_err(|error| error.to_string())?;
            let backup = backups.join(format!("{name}-{}.xml", process::unix_millis()?));
            std::fs::copy(&before, &backup)
                .map_err(|error| format!("keep {}: {error}", backup.display()))?;
            libvirt.virsh(&["define", &path_text(&after)?])?;
            drop(temporary);
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
            inspection = vm::inspect(&libvirt.virsh(&["dumpxml", "--inactive", name])?)?;
            // Every change but saving the requested size takes effect when
            // the VM next starts.
            restart_needed = fixable.iter().any(|change| !change.starts_with("save"));
        }
    }
    let problems = inspection.problems();
    if !problems.is_empty() {
        for (problem, fix) in &problems {
            ui.say(&format!("problem: {problem}"));
            ui.say(&format!("  fix: {fix}"));
        }
        return Err(format!("{name} can't be managed yet; nothing was added"));
    }
    // Without problems there is one device, and it has an alias.
    let device = inspection
        .device()
        .ok_or_else(|| format!("{name} has no virtio-mem device; nothing was added"))?;
    let alias = device
        .alias
        .clone()
        .ok_or_else(|| format!("{name}'s virtio-mem device has no alias; nothing was added"))?;

    // A VM taken out with `vm remove` comes back with the settings it had.
    if resuming {
        // Written by the earlier run; kept as it is.
    } else if parked.exists() {
        restore_parked(ui, &directory, &parked, &settings_path, &alias, options)?;
    } else {
        write_vm_settings(
            ui,
            &libvirt,
            &directory,
            &settings_path,
            &inspection,
            &alias,
            options,
        )?;
    }
    if !resuming {
        crate::audit::record(context, "vm_add", Some(name), &[]);
    }

    // A VM is attested while it runs, on the definition it was started
    // with: a changed definition needs a cold restart first.
    let running = libvirt.state(name)? == "running";
    // The definition may have been fixed by an earlier run that stopped
    // before the restart: a running VM without the device or the channel
    // its saved definition has still needs one.
    if running && !restart_needed {
        let live = vm::inspect(&libvirt.virsh(&["dumpxml", name])?)?;
        restart_needed = live.device().is_none()
            || !live.control_channel
            || live.balloon.as_deref() != inspection.balloon.as_deref();
    }
    let question = match (running, restart_needed) {
        (true, false) => None,
        (true, true) => Some(format!(
            "{name}'s definition changed. Shut it down and start it now, then apply?"
        )),
        (false, _) => Some(format!("{name} is not running. Start it now, then apply?")),
    };
    if let Some(question) = question {
        if !ui.confirm(&question)? {
            ui.say(&format!(
                "when {name} runs its saved definition, run rampire apply"
            ));
            return Ok(false);
        }
        crate::attest::cold_restart(name, &libvirt, std::time::Duration::from_secs(300))?;
        ui.say(&format!("{name} started"));
    }
    apply(context, ui)
}

fn path_text(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} is not valid UTF-8", path.display()))
}

/// Move a parked VM's settings back into `vms.d`, after checking they still
/// fit its device and name a review profile that exists.
fn restore_parked(
    ui: &mut dyn Ui,
    directory: &Path,
    parked: &Path,
    settings_path: &Path,
    alias: &str,
    options: &AddOptions,
) -> Result<(), String> {
    let name = options.vm.as_str();
    if options.has_settings() {
        return Err(format!(
            "{name}'s settings are kept in {}: add it without --review, --kernel or sizes, \
             then change them with rampire set {name} KEY=VALUE",
            parked.display()
        ));
    }
    let vm = rampire_core::settings::load_vm(parked)?;
    if vm.alias != alias {
        return Err(format!(
            "{} names the device {}, but {name}'s device is {alias}; fix the file or remove it",
            parked.display(),
            vm.alias
        ));
    }
    let review = vm.review.as_deref().ok_or_else(|| {
        format!(
            "{} names no review profile; add review = \"PROFILE\" to it",
            parked.display()
        )
    })?;
    rampire_host::attestation::read_review(
        &directory
            .join(crate::applied::REVIEW_DIRECTORY)
            .join(format!("{review}.toml")),
    )?;
    if settings_path.exists() {
        return Err(format!("{} already exists", settings_path.display()));
    }
    std::fs::rename(parked, settings_path)
        .map_err(|error| format!("move {}: {error}", parked.display()))?;
    ui.say(&format!(
        "restored {name}'s settings from {} (review {review}); change them with rampire set {name}",
        parked.display()
    ));
    Ok(())
}

/// Write a new VM's settings, with its review profile: the one named, or
/// one written from the definition and a person's answers.
fn write_vm_settings(
    ui: &mut dyn Ui,
    libvirt: &crate::attest::Libvirt,
    directory: &Path,
    settings_path: &Path,
    inspection: &crate::vm::Inspection,
    alias: &str,
    options: &AddOptions,
) -> Result<(), String> {
    use crate::vm;
    use rampire_core::settings;
    let name = options.vm.as_str();
    let review = match options.review.as_deref() {
        Some(profile) => {
            let path = directory
                .join(crate::applied::REVIEW_DIRECTORY)
                .join(format!("{profile}.toml"));
            rampire_host::attestation::read_review(&path)?;
            profile.to_owned()
        }
        None => {
            use rampire_host::attestation::AnswerSource;
            let mut answers = Vec::new();
            for detected in vm::review_answers(inspection, vm::host_has_rdma()) {
                let (value, source) = match detected.answer {
                    Some(answer) => answer,
                    None => (
                        ui.review(vm::question_text(detected.question))?,
                        AnswerSource::Confirmed,
                    ),
                };
                if !value {
                    return Err(format!(
                        "{name} can't be managed: {} is not true for it",
                        detected.question
                    ));
                }
                answers.push((detected.question, value, source));
            }
            // The kernel release: given, else read from a running guest's
            // QEMU guest agent, else asked.
            let (kernel, source) = match options.kernel.as_deref() {
                Some(kernel) => (kernel.to_owned(), AnswerSource::Confirmed),
                None => match libvirt
                    .virsh(&["qemu-agent-command", name, r#"{"execute":"guest-get-osinfo"}"#])
                    .ok()
                    .as_deref()
                    .and_then(vm::kernel_release_from_osinfo)
                {
                    Some(kernel) => {
                        ui.say(&format!(
                            "{name} runs Linux {kernel} (read from its guest agent)"
                        ));
                        (kernel, AnswerSource::Detected)
                    }
                    None => (
                        ui.text("The guest's kernel release (uname -r in the VM; its guest agent didn't answer):")?,
                        AnswerSource::Confirmed,
                    ),
                },
            };
            if kernel.is_empty() {
                return Err("a review needs the guest's kernel release".to_owned());
            }
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

    let mut suggested =
        vm::suggest(inspection, true).ok_or_else(|| format!("{name} has no virtio-mem device"))?;
    let size = |text: Option<&str>, default: u64| -> Result<u64, String> {
        text.map_or(Ok(default), |text| {
            rampire_core::parse_byte_size(text).map_err(|error| error.to_string())
        })
    };
    suggested.min_ram = size(options.min_ram.as_deref(), suggested.min_ram)?;
    suggested.max_ram = size(options.max_ram.as_deref(), suggested.max_ram)?;
    suggested.free_ram = size(options.free_ram.as_deref(), suggested.free_ram)?;
    write_new(
        settings_path,
        &vm::vm_toml(alias, options.priority, &review, &suggested),
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

/// `rampire vm remove`: park the VM's settings, or with `forget` remove
/// everything RAMpire keeps for it.
pub fn vm_remove(
    context: &Context,
    ui: &mut dyn Ui,
    name: &str,
    forget: bool,
) -> Result<(), String> {
    use rampire_core::settings;
    let directory = context.draft();
    let (path, parked) = settings_paths(&directory, name)?;
    // A VM removed earlier, now being deleted: only what was kept for it goes.
    if forget && !path.exists() && parked.exists() {
        let review = settings::load_vm(&parked)?.review;
        if !ui.confirm(&format!("Forget everything RAMpire kept for {name}?"))? {
            ui.say("nothing changed");
            return Ok(());
        }
        forget_vm(ui, &directory, name, review.as_deref())?;
        crate::audit::record(context, "vm_forget", Some(name), &[]);
        return Ok(());
    }
    let vm = settings::load_vm(&path)?;
    let question = if forget {
        format!("Stop managing {name} and forget everything RAMpire keeps for it?")
    } else {
        format!("Stop managing {name}? It keeps its memory until its next cold start.")
    };
    if !ui.confirm(&question)? {
        ui.say("nothing changed");
        return Ok(());
    }
    let libvirt = context.libvirt()?;
    // The next cold start begins on boot RAM alone (decision D18).
    if libvirt.state(name).is_ok() {
        libvirt.virsh(&[
            "update-memory-device",
            name,
            "--alias",
            &vm.alias,
            "--requested-size",
            "0",
            "--config",
        ])?;
    }
    if forget {
        std::fs::remove_file(&path)
            .map_err(|error| format!("remove {}: {error}", path.display()))?;
        crate::audit::record(context, "vm_forget", Some(name), &[]);
        // The controller lets go of the VM first; then what it kept goes.
        apply(context, ui)?;
        if let Some(copy) = still_managed_in(name)? {
            ui.say(&format!(
                "{name} is still managed by {}, so what RAMpire keeps for it stays; when it is applied, run rampire vm remove {name} --forget",
                copy.display()
            ));
            return Ok(());
        }
        forget_vm(ui, &directory, name, vm.review.as_deref())?;
        return Ok(());
    }
    std::fs::create_dir_all(directory.join(PARKED_DIRECTORY)).map_err(|error| error.to_string())?;
    std::fs::rename(&path, &parked).map_err(|error| format!("move {}: {error}", path.display()))?;
    ui.say(&format!(
        "{name}'s settings are kept in {}",
        parked.display()
    ));
    crate::audit::record(context, "vm_remove", Some(name), &[]);
    apply(context, ui)?;
    Ok(())
}

/// Remove what RAMpire keeps for a VM it no longer manages: its parked
/// settings, attestation, request state and definition backups, and its
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
    let mut gone = vec![
        directory
            .join(PARKED_DIRECTORY)
            .join(settings::vm_filename(name)?),
        rampire_host::config::attestation_path(directory, name),
    ];
    let state = Path::new(rampire_host::config::DEFAULT_STATE_DIRECTORY);
    for suffix in ["requests.json", "requests.json.lock"] {
        gone.push(state.join(format!("{name}-{suffix}")));
    }
    if let Ok(entries) = std::fs::read_dir(DOMAIN_BACKUPS) {
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            if file
                .strip_prefix(name)
                .is_some_and(|rest| rest.starts_with('-') && rest.ends_with(".xml"))
            {
                gone.push(entry.path());
            }
        }
    }
    if review == Some(name) {
        let named_elsewhere = [VMS_DIRECTORY, PARKED_DIRECTORY].iter().any(|folder| {
            std::fs::read_dir(directory.join(folder))
                .into_iter()
                .flatten()
                .flatten()
                .filter(|entry| {
                    entry.file_name() != std::ffi::OsString::from(format!("{name}.toml"))
                })
                .filter_map(|entry| settings::load_vm(&entry.path()).ok())
                .any(|other| other.review.as_deref() == Some(name))
        });
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

/// `rampire vm attest`. `vm` is the one VM, or with `drifted` every VM
/// whose live evidence has drifted.
pub fn vm_attest(
    context: &Context,
    ui: &mut dyn Ui,
    vm: Option<&str>,
    drifted: bool,
    restart: bool,
    no_apply: bool,
) -> Result<(), String> {
    use crate::attest;
    let directory = context.draft();
    let libvirt = context.libvirt()?;
    let vms = if drifted {
        let found = attest::drifted(&directory, &libvirt)?;
        if found.is_empty() {
            ui.say("no running VM has drifted from its attestation");
            return Ok(());
        }
        for (name, change) in &found {
            ui.say(&format!("{name}: {change}"));
        }
        if !ui.confirm("Attest these VMs again as they are now?")? {
            ui.say("nothing changed");
            return Ok(());
        }
        found.into_iter().map(|(name, _)| name).collect()
    } else {
        let vm = vm.ok_or("name a VM, or pass --drifted")?.to_owned();
        if restart {
            if !ui.confirm(&format!("Shut {vm} down and start it again?"))? {
                ui.say("nothing changed");
                return Ok(());
            }
            attest::cold_restart(&vm, &libvirt, std::time::Duration::from_secs(300))?;
            ui.say(&format!("{vm} restarted"));
        }
        vec![vm]
    };
    for vm in &vms {
        let path = attest::attest(&directory, vm, &libvirt)?;
        ui.say(&format!("attested {vm}: {}", path.display()));
        crate::audit::record(context, "vm_attest", Some(vm), &[]);
    }
    if no_apply {
        ui.say("not applied: run rampire apply when ready");
        return Ok(());
    }
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
    use crate::ui::{Said, Transcript};

    fn add_options(vm: &str) -> AddOptions {
        AddOptions {
            vm: vm.to_owned(),
            priority: 100,
            ..AddOptions::default()
        }
    }

    fn context(draft: &Path) -> Context {
        Context {
            config: Some(draft.to_path_buf()),
            connect: Some("test:///default".to_owned()),
            build: String::new(),
            via: "command",
        }
    }

    fn draft_with_parked_web() -> TempDir {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/examples");
        let draft = TempDir::new("test-parked").unwrap();
        for folder in [PARKED_DIRECTORY, "vms.d", "reviews"] {
            std::fs::create_dir_all(draft.path().join(folder)).unwrap();
        }
        std::fs::copy(
            examples.join("reviews/linux-standard.toml"),
            draft.path().join("reviews/linux-standard.toml"),
        )
        .unwrap();
        std::fs::copy(
            examples.join("vms.d/linux-vm.toml"),
            draft.path().join(PARKED_DIRECTORY).join("web.toml"),
        )
        .unwrap();
        draft
    }

    #[test]
    fn vm_add_restores_parked_settings_that_fit_the_device() {
        let draft = draft_with_parked_web();
        let draft = draft.path();
        let parked = draft.join(PARKED_DIRECTORY).join("web.toml");
        let settings = draft.join("vms.d/web.toml");
        let mut ui = Transcript::default();

        // Options for a new VM's settings would be ignored, so they are refused.
        let mut options = add_options("web");
        options.max_ram = Some("4GB".to_owned());
        assert!(restore_parked(
            &mut ui,
            draft,
            &parked,
            &settings,
            "ua-virtiomem0",
            &options
        )
        .is_err());
        // A different device means the file no longer describes this VM.
        assert!(restore_parked(
            &mut ui,
            draft,
            &parked,
            &settings,
            "other",
            &add_options("web")
        )
        .is_err());
        assert!(parked.exists() && !settings.exists());

        restore_parked(
            &mut ui,
            draft,
            &parked,
            &settings,
            "ua-virtiomem0",
            &add_options("web"),
        )
        .unwrap();
        assert!(!parked.exists());
        assert_eq!(
            rampire_core::settings::load_vm(&settings)
                .unwrap()
                .review
                .as_deref(),
            Some("linux-standard")
        );
        assert_eq!(
            ui.lines(),
            [format!(
                "restored web's settings from {} (review linux-standard); change them with rampire set web",
                parked.display()
            )]
        );
    }

    #[test]
    fn forgetting_a_parked_vm_asks_once_and_says_what_it_removed() {
        let draft = draft_with_parked_web();
        let parked = draft.path().join(PARKED_DIRECTORY).join("web.toml");

        let mut ui = Transcript::answering(&["n"]);
        vm_remove(&context(draft.path()), &mut ui, "web", true).unwrap();
        assert_eq!(
            ui.said,
            [
                Said::Confirm("Forget everything RAMpire kept for web?".to_owned(), false),
                Said::Line("nothing changed".to_owned()),
            ]
        );
        assert!(parked.exists());

        let mut ui = Transcript::answering(&["y"]);
        vm_remove(&context(draft.path()), &mut ui, "web", true).unwrap();
        assert_eq!(
            ui.said,
            [
                Said::Confirm("Forget everything RAMpire kept for web?".to_owned(), true),
                Said::Line(format!("removed {}", parked.display())),
            ]
        );
        assert!(!parked.exists());
        // The shared profile it named is kept: vm add didn't write it for web.
        assert!(draft.path().join("reviews/linux-standard.toml").exists());
    }

    #[test]
    fn removing_a_vm_that_isnt_managed_says_so_before_asking() {
        let draft = TempDir::new("test-unmanaged").unwrap();
        let mut ui = Transcript::default();
        let error = vm_remove(&context(draft.path()), &mut ui, "web", false).unwrap_err();
        assert!(error.contains("web.toml"), "{error}");
        assert!(ui.said.is_empty());
    }
}
