//! The controller daemon: one round of observe, allocate and resize per
//! poll, for every configured VM, until it is told to stop.

use std::path::Path;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rampire_host::attestation::{read_attestation, AttestedCompatibilitySource};
use rampire_host::config::{HostConfig, HostSettings};
use rampire_host::control_channel::ControlChannel;
use rampire_host::guest_channel::{publication_copy_path, GuestChannel};
use rampire_host::host_memory::ProcMeminfoSource;
use rampire_host::ownership::DeviceOwner;
use rampire_host::qga::VirshGuestFiles;
use rampire_host::resize_sink::VirshResizeSink;
use rampire_host::runtime::{HostRuntime, VmController};
use rampire_host::virsh::Virsh;
use rampire_host::xml_source::VirshXmlSource;
use signal_hook::consts::signal::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::flag;

/// The exit status for a failure at start that is safe to try again later
/// (EX_TEMPFAIL): rampire.service restarts on it, and on nothing else.
pub const RETRY_LATER: u8 = 75;

/// The copy the running controller started on, which `rampire apply` keeps
/// while it runs.
pub use rampire_ops::context::RUNNING_COPY;

/// `rampire daemon`: run the controller on the configuration in
/// `config_path` until SIGTERM or SIGINT. SIGHUP reloads it.
pub fn run(config_path: &Path) -> ExitCode {
    let link = config_path.to_path_buf();
    // Resolve the applied copy's `current` link once: a later apply then
    // can't change the files under a running controller, which keeps the
    // copy it started on until it restarts.
    let config_path = match config_path.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            eprintln!(
                "rampire configuration error: {}: {error} (has `rampire apply` run?)",
                config_path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let config_path = config_path.as_path();
    // Only `apply`'s pruning reads this, so the controller runs without it,
    // but a person should know the running copy may be pruned.
    if let Err(error) = std::fs::write(RUNNING_COPY, format!("{}\n", config_path.display())) {
        eprintln!(
            "rampire: warning: can't record the running copy in {RUNNING_COPY}: {error}; rampire apply may prune {}",
            config_path.display()
        );
    }
    let settings = match HostSettings::load(config_path) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("rampire configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    if let Err(error) = flag::register(SIGTERM, Arc::clone(&stop))
        .and_then(|_| flag::register(SIGINT, Arc::clone(&stop)))
    {
        eprintln!("rampire signal setup error: {error}");
        return ExitCode::FAILURE;
    }
    if settings.vms.is_empty() {
        // A configuration with no VMs is valid: a freshly
        // installed host runs, idle, until a VM is added.
        eprintln!("event=no_vms rampire manages no VMs yet; add one with: rampire vm add VM");
        while !stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(200));
        }
        eprintln!("rampire stopped");
        return ExitCode::SUCCESS;
    }
    // Timeouts and the connection are global settings, shared by every VM.
    // Everything the controller asks libvirt goes over one connection, and
    // libvirt asks polkit only when a connection opens: once it is open,
    // polkit refusing checks (out of open files, restarting) can't stop the
    // controller. Only the event subscription is a `virsh` process of its own.
    let virsh = Virsh::persistent_with_connection(
        settings.virsh_binary.clone(),
        settings.command_timeout,
        settings.libvirt_uri.clone(),
    );
    if let Err(error) = virsh.require_management_access() {
        // Nothing has been done yet, so trying again is safe: polkit may be
        // restarting or reloading its rules (a package upgrade does that).
        // The unit restarts on this status alone; every other failure stays
        // fail-stop.
        // polkit 125 keeps a file open for every check it is asked; with
        // none left it refuses them all, which reads as "Not authorized".
        eprintln!(
            "rampire authorization error: {error}; trying again in 30 s (exit {RETRY_LATER}). If polkit has run out of open files, rampire doctor says so; systemctl restart polkit fixes it"
        );
        return ExitCode::from(RETRY_LATER);
    }
    // Device owners are held until the runtime ends.
    let mut owners = Vec::with_capacity(settings.vms.len());
    let mut vms = Vec::with_capacity(settings.vms.len());
    let mut controls = Vec::with_capacity(settings.vms.len());
    for config in settings.vms {
        let name = config.vm_name.clone();
        let control = ControlChannel::new(virsh.clone(), name.clone());
        match vm_controller(&virsh, config, Arc::clone(&control)) {
            Ok((owner, vm)) => {
                owners.push(owner);
                vms.push(vm);
                controls.push(control);
            }
            // Nothing has been done to a VM that can't be set up (libvirt no
            // longer has it, or it lost its device): it is left alone, and
            // the others are still managed. One such VM used to stop the
            // controller for every VM. `rampire doctor VM` says what is wrong.
            Err(error) => {
                eprintln!("rampire: vm={name} event=vm_setup_failed reason={error}");
            }
        }
    }
    if vms.is_empty() {
        // Nothing to manage, and nothing done: try again as a fixed
        // configuration or libvirt may allow.
        eprintln!("rampire: no VM could be set up; trying again in 30 s (exit {RETRY_LATER})");
        return ExitCode::from(RETRY_LATER);
    }
    let mut runtime = match HostRuntime::new(vms, ProcMeminfoSource::new(), settings.pool_bytes) {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("rampire configuration error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let events = match rampire_host::wake::MemoryEvents::start(&virsh) {
        Ok(events) => Some(events),
        Err(error) => {
            eprintln!("libvirt events unavailable; polling continues: {error}");
            None
        }
    };
    let reload = Arc::new(AtomicBool::new(false));
    if let Err(error) = flag::register(SIGHUP, Arc::clone(&reload)) {
        eprintln!("rampire signal setup error: {error}");
        return ExitCode::FAILURE;
    }
    // A reload resolves the applied copy's link again.
    let load = Box::new(move || {
        let path = link
            .canonicalize()
            .map_err(|error| format!("{}: {error}", link.display()))?;
        HostSettings::load(&path).map_err(|error| error.to_string())
    });
    runtime = runtime
        .with_reload(reload, load)
        .with_wake(controls, events)
        .with_metrics(
            std::path::PathBuf::from(rampire_host::metrics::METRICS_PATH),
            crate::version(),
        );
    let result = runtime.run(&stop);
    drop(owners);
    match result {
        Ok(()) => {
            eprintln!("rampire stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rampire failed: {error}");
            ExitCode::FAILURE
        }
    }
}

type Controller = VmController<
    GuestChannel<VirshGuestFiles<Virsh>>,
    VirshXmlSource<Virsh>,
    VirshResizeSink<Virsh, AttestedCompatibilitySource<Virsh>>,
>;

fn vm_controller(
    virsh: &Virsh,
    config: HostConfig,
    control: Arc<ControlChannel>,
) -> Result<(DeviceOwner, Controller), String> {
    let owner = DeviceOwner::for_domain(virsh, &config.vm_name)?;
    let fingerprint =
        read_attestation(Path::new(&config.compatibility_attestation_path))?.fingerprint_sha256;
    let controller = VmController::new(
        GuestChannel::new(
            VirshGuestFiles::new(virsh.clone(), config.vm_name.clone()),
            &config.raw_telemetry_path,
            &config.vm_name,
            &config.raw_telemetry_service_name,
            config.raw_telemetry_max_age,
            config.raw_telemetry_future_tolerance,
        )
        .with_control(control)
        .with_publication_copy(publication_copy_path(&config.vm_name)),
        VirshXmlSource::new(virsh.clone(), config.vm_name.clone(), config.alias.clone()),
        VirshResizeSink::new(
            virsh.clone(),
            config.vm_name.clone(),
            config.alias.clone(),
            // Over the shared connection too, which reads the evidence byte
            // for byte as `virsh` prints it (`vm add` records it the same
            // way). Run as `virsh`, each read before a resize was a polkit
            // check, and polkit refusing one latched the VM.
            AttestedCompatibilitySource::new(
                virsh.clone(),
                config.vm_name.clone(),
                config.alias.clone(),
                config.guest_os,
                config.compatibility_attestation_path.clone(),
            ),
        ),
        config,
        fingerprint,
    )?;
    Ok((owner, controller))
}
