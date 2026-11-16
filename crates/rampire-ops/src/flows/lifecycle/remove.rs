//! `rampire vm remove`: letting a VM go, its definition back to plain RAM
//! and everything RAMpire kept for it removed.

use super::*;

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
    without_settings(&path, name, || {
        crate::flows::apply::apply_agreed(context, ui)
    })?;
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

/// The applied copy, current or running, that still manages `name`. Its
/// checkpoint and lock belong to the controller until neither does.
pub(super) fn still_managed_in(name: &str) -> Result<Option<PathBuf>, String> {
    use rampire_core::settings::{self, VMS_DIRECTORY};
    let filename = settings::vm_filename(name)?;
    let running = running_copy();
    let current = crate::applied::current(applied_root())?;
    Ok([current, running]
        .into_iter()
        .flatten()
        .find(|copy| copy.join(VMS_DIRECTORY).join(&filename).exists()))
}

#[cfg(test)]
mod tests {
    use super::super::tests::context;
    use super::*;
    use crate::ui::Transcript;

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
}
