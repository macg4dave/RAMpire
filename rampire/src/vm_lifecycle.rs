//! `vm add`, `vm remove` and `vm attest`: bringing a VM under management
//! (fixing its definition, writing its review and settings, restoring
//! parked settings), parking or forgetting it, and attesting it again.

use std::io::{IsTerminal, Write};
use std::path::Path;
use std::process::ExitCode;

use rampire_ops::process;

use crate::apply::{apply, running_copy};
use crate::{applied_root, confirm, draft, libvirt, path_text, Cli, TempDir};

pub(crate) struct AddOptions<'a> {
    pub(crate) vm: &'a str,
    pub(crate) fix_xml: bool,
    pub(crate) review: Option<&'a str>,
    pub(crate) kernel: Option<&'a str>,
    pub(crate) priority: u32,
    pub(crate) min_ram: Option<&'a str>,
    pub(crate) max_ram: Option<&'a str>,
    pub(crate) free_ram: Option<&'a str>,
}

/// Ask a person on the terminal; without one, there is nobody to answer.
fn ask(question: &str) -> Result<String, String> {
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "{question} needs a person: answer on a terminal, or name a reviewed profile with --review"
        ));
    }
    print!("{question} ");
    std::io::stdout()
        .flush()
        .map_err(|error| error.to_string())?;
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|error| error.to_string())?;
    Ok(answer.trim().to_owned())
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

pub(crate) fn vm_add(cli: &Cli, options: &AddOptions) -> Result<ExitCode, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    use rampire_ops::vm;
    let name = options.vm;
    let directory = draft(cli);
    let filename = settings::vm_filename(name)?;
    let settings_path = directory.join(VMS_DIRECTORY).join(&filename);
    // Settings in the draft but not in the applied copy are an earlier
    // `vm add` that stopped before applying: this run finishes it.
    let resuming = settings_path.exists();
    if resuming {
        let applied = rampire_ops::applied::current(applied_root())?
            .map(|current| current.join(VMS_DIRECTORY).join(&filename));
        if applied.is_some_and(|path| path.exists()) {
            return Err(format!(
                "{name} is already managed; change it with rampire set {name} KEY=VALUE"
            ));
        }
        if options.review.is_some()
            || options.kernel.is_some()
            || options.min_ram.is_some()
            || options.max_ram.is_some()
            || options.free_ram.is_some()
        {
            return Err(format!(
                "{name} has settings from an earlier vm add in {}: run vm add {name} without settings to finish it, or change them with rampire set",
                settings_path.display()
            ));
        }
        println!(
            "{name} has settings from an earlier vm add that didn't finish ({}); finishing it",
            settings_path.display()
        );
    }
    let libvirt = libvirt(cli)?;
    let xml = libvirt.virsh(&["dumpxml", "--inactive", name])?;
    let mut inspection = vm::inspect(&xml)?;
    let mut restart_needed = false;

    let fixable = inspection.fixable();
    if !fixable.is_empty() {
        for change in &fixable {
            println!("to fix: {change}");
        }
        if !options.fix_xml {
            println!("rampire vm add {name} --fix-xml makes these changes");
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
            print!("{}", String::from_utf8_lossy(&shown.stdout));
            if !confirm(cli, &format!("Change {name}'s saved definition like this?"))? {
                println!("nothing changed");
                return Ok(ExitCode::SUCCESS);
            }
            let backups = Path::new(DOMAIN_BACKUPS);
            std::fs::create_dir_all(backups).map_err(|error| error.to_string())?;
            let backup = backups.join(format!("{name}-{}.xml", process::unix_millis()?));
            std::fs::copy(&before, &backup)
                .map_err(|error| format!("keep {}: {error}", backup.display()))?;
            libvirt.virsh(&["define", &path_text(&after)?])?;
            drop(temporary);
            println!(
                "changed {name}'s definition; the old one is in {}",
                backup.display()
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
            println!("problem: {problem}");
            println!("  fix: {fix}");
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
    let parked = directory
        .join(PARKED_DIRECTORY)
        .join(settings::vm_filename(name)?);
    if resuming {
        // Written by the earlier run; kept as it is.
    } else if parked.exists() {
        restore_parked(&directory, &parked, &settings_path, &alias, options)?;
    } else {
        write_vm_settings(
            &libvirt,
            &directory,
            &settings_path,
            &inspection,
            &alias,
            options,
        )?;
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
        if !confirm(cli, &question)? {
            println!("when {name} runs its saved definition, run rampire apply");
            return Ok(ExitCode::SUCCESS);
        }
        rampire_ops::attest::cold_restart(name, &libvirt, std::time::Duration::from_secs(300))?;
        println!("{name} started");
    }
    apply(cli, false)
}

/// Where `vm remove` keeps a VM's settings, in the draft.
const PARKED_DIRECTORY: &str = "parked";

/// Move a parked VM's settings back into `vms.d`, after checking they still
/// fit its device and name a review profile that exists.
fn restore_parked(
    directory: &Path,
    parked: &Path,
    settings_path: &Path,
    alias: &str,
    options: &AddOptions,
) -> Result<(), String> {
    let name = options.vm;
    if options.review.is_some()
        || options.kernel.is_some()
        || options.min_ram.is_some()
        || options.max_ram.is_some()
        || options.free_ram.is_some()
    {
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
            .join(rampire_ops::applied::REVIEW_DIRECTORY)
            .join(format!("{review}.toml")),
    )?;
    if settings_path.exists() {
        return Err(format!("{} already exists", settings_path.display()));
    }
    std::fs::rename(parked, settings_path)
        .map_err(|error| format!("move {}: {error}", parked.display()))?;
    println!(
        "restored {name}'s settings from {} (review {review}); change them with rampire set {name}",
        parked.display()
    );
    Ok(())
}

/// Write a new VM's settings, with its review profile: the one named, or
/// one written from the definition and a person's answers.
fn write_vm_settings(
    libvirt: &rampire_ops::attest::Libvirt,
    directory: &Path,
    settings_path: &Path,
    inspection: &rampire_ops::vm::Inspection,
    alias: &str,
    options: &AddOptions,
) -> Result<(), String> {
    use rampire_core::settings;
    use rampire_ops::vm;
    let name = options.vm;
    let review = match options.review {
        Some(profile) => {
            let path = directory
                .join(rampire_ops::applied::REVIEW_DIRECTORY)
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
                    None => {
                        let reply =
                            ask(&format!("{} [y/N]", vm::question_text(detected.question)))?;
                        (
                            matches!(reply.as_str(), "y" | "Y" | "yes" | "Yes"),
                            AnswerSource::Confirmed,
                        )
                    }
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
            let (kernel, source) = match options.kernel {
                Some(kernel) => (kernel.to_owned(), AnswerSource::Confirmed),
                None => match libvirt
                    .virsh(&["qemu-agent-command", name, r#"{"execute":"guest-get-osinfo"}"#])
                    .ok()
                    .as_deref()
                    .and_then(vm::kernel_release_from_osinfo)
                {
                    Some(kernel) => {
                        println!("{name} runs Linux {kernel} (read from its guest agent)");
                        (kernel, AnswerSource::Detected)
                    }
                    None => (
                        ask("The guest's kernel release (uname -r in the VM; its guest agent didn't answer):")?,
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
                .join(rampire_ops::applied::REVIEW_DIRECTORY)
                .join(format!("{name}.toml"));
            write_new(&path, &text)?;
            rampire_host::attestation::read_review(&path)?;
            println!("wrote the review {}", path.display());
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
    suggested.min_ram = size(options.min_ram, suggested.min_ram)?;
    suggested.max_ram = size(options.max_ram, suggested.max_ram)?;
    suggested.free_ram = size(options.free_ram, suggested.free_ram)?;
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
    println!("wrote {}", settings_path.display());
    Ok(())
}

pub(crate) fn vm_remove(cli: &Cli, name: &str, forget: bool) -> Result<ExitCode, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    let directory = draft(cli);
    let path = directory
        .join(VMS_DIRECTORY)
        .join(settings::vm_filename(name)?);
    // A VM removed earlier, now being deleted: only what was kept for it goes.
    let parked = directory
        .join(PARKED_DIRECTORY)
        .join(settings::vm_filename(name)?);
    if forget && !path.exists() && parked.exists() {
        let review = settings::load_vm(&parked)?.review;
        if !confirm(cli, &format!("Forget everything RAMpire kept for {name}?"))? {
            println!("nothing changed");
            return Ok(ExitCode::SUCCESS);
        }
        forget_vm(&directory, name, review.as_deref())?;
        return Ok(ExitCode::SUCCESS);
    }
    let vm = settings::load_vm(&path)?;
    let question = if forget {
        format!("Stop managing {name} and forget everything RAMpire keeps for it?")
    } else {
        format!("Stop managing {name}? It keeps its memory until its next cold start.")
    };
    if !confirm(cli, &question)? {
        println!("nothing changed");
        return Ok(ExitCode::SUCCESS);
    }
    let libvirt = libvirt(cli)?;
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
        // The controller lets go of the VM first; then what it kept goes.
        let code = apply(cli, false)?;
        if let Some(copy) = still_managed_in(name)? {
            println!(
                "{name} is still managed by {}, so what RAMpire keeps for it stays;                  when it is applied, run rampire vm remove {name} --forget",
                copy.display()
            );
            return Ok(code);
        }
        forget_vm(&directory, name, vm.review.as_deref())?;
        return Ok(code);
    }
    std::fs::create_dir_all(directory.join(PARKED_DIRECTORY)).map_err(|error| error.to_string())?;
    std::fs::rename(&path, &parked).map_err(|error| format!("move {}: {error}", path.display()))?;
    println!("{name}'s settings are kept in {}", parked.display());
    apply(cli, false)
}

/// Remove what RAMpire keeps for a VM it no longer manages: its parked
/// settings, attestation, request state and definition backups, and its
/// review profile when `vm add` wrote it for this VM (named after it) and
/// no other VM's settings name it.
fn forget_vm(directory: &Path, name: &str, review: Option<&str>) -> Result<(), String> {
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
                    .join(rampire_ops::applied::REVIEW_DIRECTORY)
                    .join(format!("{name}.toml")),
            );
        }
    }
    for path in gone {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("removed {}", path.display()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("remove {}: {error}", path.display())),
        }
    }
    Ok(())
}

/// The applied copy, current or running, that still manages `name`. Its
/// checkpoint and lock belong to the controller until neither does.
fn still_managed_in(name: &str) -> Result<Option<std::path::PathBuf>, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    let filename = settings::vm_filename(name)?;
    let running = running_copy();
    let current = rampire_ops::applied::current(applied_root())?;
    Ok([current, running]
        .into_iter()
        .flatten()
        .find(|copy| copy.join(VMS_DIRECTORY).join(&filename).exists()))
}

/// Where `vm add --fix-xml` keeps the definitions it replaced.
const DOMAIN_BACKUPS: &str = "/var/lib/rampire/domain-backups";

pub(crate) fn vm_attest(
    cli: &Cli,
    vm: Option<&str>,
    drifted: bool,
    restart: bool,
    no_apply: bool,
) -> Result<ExitCode, String> {
    use rampire_ops::attest;
    let directory = draft(cli);
    let libvirt = libvirt(cli)?;
    let vms = if drifted {
        let found = attest::drifted(&directory, &libvirt)?;
        if found.is_empty() {
            println!("no running VM has drifted from its attestation");
            return Ok(ExitCode::SUCCESS);
        }
        for (name, change) in &found {
            println!("{name}: {change}");
        }
        if !confirm(cli, "Attest these VMs again as they are now?")? {
            println!("nothing changed");
            return Ok(ExitCode::SUCCESS);
        }
        found.into_iter().map(|(name, _)| name).collect()
    } else {
        let vm = vm.ok_or("name a VM, or pass --drifted")?.to_owned();
        if restart {
            if !confirm(cli, &format!("Shut {vm} down and start it again?"))? {
                println!("nothing changed");
                return Ok(ExitCode::SUCCESS);
            }
            attest::cold_restart(&vm, &libvirt, std::time::Duration::from_secs(300))?;
            println!("{vm} restarted");
        }
        vec![vm]
    };
    for vm in &vms {
        let path = attest::attest(&directory, vm, &libvirt)?;
        println!("attested {vm}: {}", path.display());
    }
    if no_apply {
        println!("not applied: run rampire apply when ready");
        return Ok(ExitCode::SUCCESS);
    }
    apply(cli, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_options(vm: &str) -> AddOptions<'_> {
        AddOptions {
            vm,
            fix_xml: false,
            review: None,
            kernel: None,
            priority: 100,
            min_ram: None,
            max_ram: None,
            free_ram: None,
        }
    }

    #[test]
    fn vm_add_restores_parked_settings_that_fit_the_device() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../deploy/examples");
        let draft = std::env::temp_dir().join(format!("rampire-parked-{}", std::process::id()));
        for folder in [PARKED_DIRECTORY, "vms.d", "reviews"] {
            std::fs::create_dir_all(draft.join(folder)).unwrap();
        }
        std::fs::copy(
            examples.join("reviews/linux-standard.toml"),
            draft.join("reviews/linux-standard.toml"),
        )
        .unwrap();
        let parked = draft.join(PARKED_DIRECTORY).join("web.toml");
        let settings = draft.join("vms.d/web.toml");
        std::fs::copy(examples.join("vms.d/linux-vm.toml"), &parked).unwrap();

        // Options for a new VM's settings would be ignored, so they are refused.
        let mut options = add_options("web");
        options.max_ram = Some("4GB");
        assert!(restore_parked(&draft, &parked, &settings, "ua-virtiomem0", &options).is_err());
        // A different device means the file no longer describes this VM.
        assert!(restore_parked(&draft, &parked, &settings, "other", &add_options("web")).is_err());
        assert!(parked.exists() && !settings.exists());

        restore_parked(
            &draft,
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
        std::fs::remove_dir_all(&draft).unwrap();
    }
}
