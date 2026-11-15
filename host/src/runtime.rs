//! The host control loop. Each guest decides when to add or remove memory
//! and publishes one request at a time. The runtime admits it, grants what is
//! free (pool bytes by priority, device ceiling, host reserve), records the
//! admission and command intent durably, resizes, and publishes the actual
//! result once live state converges. It never estimates guest demand and
//! never queues the unfulfilled remainder of a request.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use rampire_core::{allocate_pool, service_order};
use thiserror::Error;

use crate::guest_channel::GuestLink;
use crate::host_memory::HostMemorySource;
use crate::resize_sink::ResizeSink;
use crate::xml_source::MemoryStateSource;

mod vm_controller;

pub use vm_controller::VmController;
use vm_controller::{HostBudget, Observed};

#[derive(Debug, Error)]
pub enum HostRuntimeError {
    #[error("memory controller configuration is invalid: {0}")]
    Controller(String),
    #[error("host available-memory check failed: {0}")]
    HostMemory(String),
    #[error("resize request failed: {0}")]
    Resize(String),
    #[error("virtio-mem request did not converge within {0:?}")]
    ConvergenceTimeout(Duration),
}

/// What the loop does after one poll.
enum Step {
    Wait,
    Stop(HostRuntimeError),
}

/// A reload request and how to load the configuration it asks for.
struct Reload {
    requested: std::sync::Arc<AtomicBool>,
    load: Box<dyn FnMut() -> Result<crate::config::HostSettings, String>>,
}

/// The metrics file and what it needs between rounds.
struct MetricsFile {
    path: std::path::PathBuf,
    build: String,
    rounds: u64,
    /// A write failure is logged once, not every round.
    failure_reported: bool,
}

pub struct HostRuntime<G, S, R, H> {
    vms: Vec<VmController<G, S, R>>,
    host_memory: H,
    /// Shared device-byte budget; `None` leaves each VM bounded only by its
    /// device and the host reserve.
    pool_bytes: Option<u64>,
    poll_interval: Duration,
    /// Where each round's metrics go, if anywhere.
    metrics: Option<MetricsFile>,
    /// Set by SIGHUP: load the applied configuration again.
    reload: Option<Reload>,
    #[cfg(unix)]
    controls: Vec<std::sync::Arc<crate::control_channel::ControlChannel>>,
    #[cfg(unix)]
    memory_events: Option<crate::wake::MemoryEvents>,
}

impl<G, S, R, H> HostRuntime<G, S, R, H>
where
    G: GuestLink,
    S: MemoryStateSource,
    R: ResizeSink,
    H: HostMemorySource,
{
    /// The poll interval is a global setting, so every member carries the same one.
    pub fn new(
        vms: Vec<VmController<G, S, R>>,
        host_memory: H,
        pool_bytes: Option<u64>,
    ) -> Result<Self, HostRuntimeError> {
        let poll_interval = vms
            .first()
            .map(|vm| vm.config.poll_interval)
            .ok_or_else(|| HostRuntimeError::Controller("no VMs to control".to_owned()))?;
        Ok(Self {
            vms,
            host_memory,
            pool_bytes,
            poll_interval,
            metrics: None,
            reload: None,
            #[cfg(unix)]
            controls: Vec::new(),
            #[cfg(unix)]
            memory_events: None,
        })
    }

    #[cfg(unix)]
    pub fn with_wake(
        mut self,
        controls: Vec<std::sync::Arc<crate::control_channel::ControlChannel>>,
        events: Option<crate::wake::MemoryEvents>,
    ) -> Self {
        self.controls = controls;
        self.memory_events = events;
        self
    }

    pub fn run(&mut self, stop: &AtomicBool) -> Result<(), HostRuntimeError> {
        while !stop.load(Ordering::Acquire) {
            self.reload_if_requested();
            if let Step::Stop(error) = self.round(stop)? {
                return Err(error);
            }
            #[cfg(unix)]
            if !self.controls.is_empty() {
                crate::wake::wait(
                    &self.controls,
                    self.memory_events.as_mut(),
                    stop,
                    self.poll_interval,
                );
                continue;
            }
            wait_interruptibly(stop, self.poll_interval);
        }
        Ok(())
    }

    /// Reload when `requested` is set, from what `load` returns.
    pub fn with_reload(
        mut self,
        requested: std::sync::Arc<AtomicBool>,
        load: Box<dyn FnMut() -> Result<crate::config::HostSettings, String>>,
    ) -> Self {
        self.reload = Some(Reload { requested, load });
        self
    }

    fn reload_if_requested(&mut self) {
        let Some(reload) = self.reload.as_mut() else {
            return;
        };
        if !reload.requested.swap(false, Ordering::AcqRel) {
            return;
        }
        match (reload.load)().and_then(|settings| self.reload(settings)) {
            Ok(()) => eprintln!("rampire: event=config_reloaded"),
            Err(error) => eprintln!(
                "rampire: event=reload_refused reason={error}; the controller keeps its configuration, restart it to take the new one"
            ),
        }
    }

    /// Take a new configuration without a restart, when it changes only
    /// what can change while running: priorities, limits, the pool, the
    /// host reserve. Each VM keeps the attestation file it started with,
    /// whose bytes are checked to be the same.
    pub fn reload(&mut self, settings: crate::config::HostSettings) -> Result<(), String> {
        let names: Vec<_> = self.vms.iter().map(|vm| &vm.config.vm_name).collect();
        let new_names: Vec<_> = settings.vms.iter().map(|vm| &vm.vm_name).collect();
        if names != new_names {
            return Err("VMs were added or removed".to_owned());
        }
        for (vm, new) in self.vms.iter().zip(&settings.vms) {
            crate::config::reloadable(&vm.config, new)?;
        }
        for (vm, new) in self.vms.iter_mut().zip(settings.vms) {
            let attestation = std::mem::take(&mut vm.config.compatibility_attestation_path);
            vm.config = crate::config::HostConfig {
                compatibility_attestation_path: attestation,
                ..new
            };
        }
        self.pool_bytes = settings.pool_bytes;
        Ok(())
    }

    /// Write each round's metrics to `path`.
    pub fn with_metrics(mut self, path: std::path::PathBuf, build: String) -> Self {
        self.metrics = Some(MetricsFile {
            path,
            build,
            rounds: 0,
            failure_reported: false,
        });
        self
    }

    fn write_metrics(&mut self, observed: &[Observed]) {
        let Some(file) = self.metrics.as_mut() else {
            return;
        };
        file.rounds += 1;
        let vms: Vec<_> = self
            .vms
            .iter()
            .zip(observed)
            .map(|(vm, seen)| crate::metrics::VmMetrics {
                name: vm.config.vm_name.clone(),
                priority: vm.config.priority,
                charged_bytes: match seen {
                    Observed::Held { charge } => charge.map(|(bytes, _)| bytes),
                    Observed::Ready(ready) => {
                        Some(ready.state.current_bytes.max(ready.state.requested_bytes))
                    }
                    Observed::Stop(_) => None,
                },
                max_ram_bytes: vm.config.max_ram,
                stopped: vm.stopped,
                latched: vm.control.latched(),
            })
            .collect();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        let text = crate::metrics::render(&file.build, self.pool_bytes, file.rounds, now, &vms);
        match crate::metrics::write(&file.path, &text) {
            Ok(()) => file.failure_reported = false,
            Err(error) if !file.failure_reported => {
                eprintln!("rampire: event=metrics_unwritten reason={error}");
                file.failure_reported = true;
            }
            Err(_) => {}
        }
    }

    /// Observe every VM, allocate the pool, then execute each ready request,
    /// highest priority first. A VM that fails (a resize that never
    /// converges, an unknown growth outcome) is latched and stops; the others
    /// keep running. The daemon stops only once every VM has.
    fn round(&mut self, stop: &AtomicBool) -> Result<Step, HostRuntimeError> {
        let mut last_stop = None;
        let mut observed = Vec::with_capacity(self.vms.len());
        for vm in &mut self.vms {
            if stop.load(Ordering::Acquire) {
                return Ok(Step::Wait);
            }
            let seen = if vm.stopped {
                vm.observe_stopped()
            } else {
                vm.observe(stop)?
            };
            match seen {
                Observed::Stop(error) => {
                    vm.stop(&error);
                    last_stop = Some(error);
                    observed.push(vm.observe_stopped());
                }
                other => observed.push(other),
            }
        }
        if stop.load(Ordering::Acquire) {
            return Ok(Step::Wait);
        }
        let ceilings = self.allocate(&observed)?;
        self.write_metrics(&observed);
        // The same order the pool used: priority, then first come, first
        // served, then configuration order.
        let mut order = (0..self.vms.len()).collect::<Vec<_>>();
        order.sort_by_key(|index| {
            service_order(&self.vms[*index].pool_member(&observed[*index], u64::MAX))
        });
        let mut host = HostBudget {
            committed: self.vms.iter().fold(0_u64, |total, vm| {
                total.saturating_add(vm.outstanding_growth.unwrap_or(u64::MAX))
            }),
        };
        let mut observed = observed.into_iter().map(Some).collect::<Vec<_>>();
        for index in order {
            if stop.load(Ordering::Acquire) {
                break;
            }
            let Some(Observed::Ready(ready)) = observed[index].take() else {
                continue;
            };
            let ceiling = ceilings.as_ref().map(|ceilings| ceilings[index]);
            let vm = &mut self.vms[index];
            if let Step::Stop(error) = vm.act(ready, ceiling, &self.host_memory, &mut host, stop)? {
                vm.stop(&error);
                last_stop = Some(error);
            }
        }
        match last_stop {
            Some(error) if self.vms.iter().all(|vm| vm.stopped) => Ok(Step::Stop(error)),
            _ => Ok(Step::Wait),
        }
    }

    fn allocate(&self, observed: &[Observed]) -> Result<Option<Vec<u64>>, HostRuntimeError> {
        let Some(pool_bytes) = self.pool_bytes else {
            return Ok(None);
        };
        let members = self
            .vms
            .iter()
            .zip(observed)
            .map(|(vm, observed)| vm.pool_member(observed, pool_bytes))
            .collect::<Vec<_>>();
        let allocations = allocate_pool(pool_bytes, &members)
            .map_err(|error| HostRuntimeError::Controller(error.to_string()))?;
        Ok(Some(
            allocations
                .into_iter()
                .map(|allocation| allocation.ceiling_bytes)
                .collect(),
        ))
    }
}

fn wait_interruptibly(stop: &AtomicBool, duration: Duration) {
    let Some(deadline) = Instant::now().checked_add(duration) else {
        return;
    };
    while !stop.load(Ordering::Acquire) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        thread::sleep(remaining.min(Duration::from_millis(100)));
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use rampire_core::{
        CommandDirection, HostMemoryStatus, MemoryOperation, MemoryOutcome, MemoryRequest,
        MemoryResourceNotificationState, MemoryResult, MemoryTelemetrySnapshot,
        RawTelemetryEnvelope, RequestIdentity, VirtioMemState, MEMORY_REQUEST_VERSION,
    };

    use super::*;
    use crate::checkpoint::{Checkpoint, CommandIntent, DurableControlState};
    use crate::config::HostConfig;
    use crate::guest_channel::{SystemUnixClock, UnixClock};
    use crate::resize_sink::ResizeSinkError;
    use rampire_core::settings::GuestOs;

    use rampire_core::{GIB, MIB};

    struct DeviceState {
        live: VirtioMemState,
        /// A shrink stops returning memory at this allocation.
        stall_at: Option<u64>,
        /// A growth stops plugging memory at this allocation.
        grow_stall_at: Option<u64>,
    }

    /// A device that converges toward `requested` whenever it is read.
    #[derive(Clone)]
    struct Device(Rc<RefCell<DeviceState>>);

    impl Device {
        fn new(current_bytes: u64) -> Self {
            Self(Rc::new(RefCell::new(DeviceState {
                live: VirtioMemState {
                    size_bytes: 32 * GIB,
                    block_size_bytes: 2 * MIB,
                    requested_bytes: current_bytes,
                    current_bytes,
                },
                stall_at: None,
                grow_stall_at: None,
            })))
        }

        fn live(&self) -> VirtioMemState {
            self.0.borrow().live
        }
    }

    impl MemoryStateSource for Device {
        fn memory_state(&self) -> Result<VirtioMemState, String> {
            let mut device = self.0.borrow_mut();
            let requested = device.live.requested_bytes;
            device.live.current_bytes = if requested >= device.live.current_bytes {
                requested
                    .min(device.grow_stall_at.unwrap_or(u64::MAX))
                    .max(device.live.current_bytes)
            } else {
                requested.max(device.stall_at.unwrap_or(0))
            };
            Ok(device.live)
        }
    }

    struct Sink {
        device: Device,
        targets: Rc<RefCell<Vec<u64>>>,
        failure: Option<ResizeSinkError>,
    }

    impl ResizeSink for Sink {
        fn request_resize_guarded(
            &self,
            expected: VirtioMemState,
            target: u64,
            stop: &AtomicBool,
            authorize: &dyn Fn() -> Result<(), String>,
        ) -> Result<(), ResizeSinkError> {
            if self.device.live() != expected {
                return Err(ResizeSinkError::Rejected("state changed".to_owned()));
            }
            authorize().map_err(ResizeSinkError::Rejected)?;
            if stop.load(Ordering::Acquire) {
                return Err(ResizeSinkError::Rejected("shutdown requested".to_owned()));
            }
            match &self.failure {
                Some(ResizeSinkError::Rejected(reason)) => {
                    return Err(ResizeSinkError::Rejected(reason.clone()))
                }
                // An unknown outcome that did in fact take effect.
                Some(ResizeSinkError::CommandUnknown(reason)) => {
                    self.device.0.borrow_mut().live.requested_bytes = target;
                    self.targets.borrow_mut().push(target);
                    return Err(ResizeSinkError::CommandUnknown(reason.clone()));
                }
                None => {}
            }
            self.device.0.borrow_mut().live.requested_bytes = target;
            self.targets.borrow_mut().push(target);
            Ok(())
        }

        fn retarget_pending_guarded(
            &self,
            _expected: VirtioMemState,
            target: u64,
            _stop: &AtomicBool,
        ) -> Result<(), ResizeSinkError> {
            let mut device = self.device.0.borrow_mut();
            device.live.requested_bytes = target;
            self.targets.borrow_mut().push(target);
            Ok(())
        }
    }

    #[derive(Default)]
    struct GuestState {
        request: Option<MemoryRequest>,
        statuses: Vec<HostMemoryStatus>,
        native: Option<MemoryResourceNotificationState>,
        /// Report Windows' total as fixed RAM plus this device's current
        /// allocation, as a real guest does; otherwise a fixed 8 GiB.
        device: Option<Device>,
        /// Publish nothing, as a guest still waiting for its settings.
        silent: bool,
    }

    #[derive(Clone, Default)]
    struct Guest(Rc<RefCell<GuestState>>);

    impl Guest {
        fn send(&self, request: Option<MemoryRequest>) {
            self.0.borrow_mut().request = request;
        }

        fn signal(&self, state: MemoryResourceNotificationState) {
            self.0.borrow_mut().native = Some(state);
        }

        fn track(&self, device: &Device) {
            self.0.borrow_mut().device = Some(device.clone());
        }

        fn last_result(&self) -> Option<MemoryResult> {
            self.0
                .borrow()
                .statuses
                .last()
                .and_then(|status| status.result.clone())
        }
    }

    impl GuestLink for Guest {
        fn read(&self) -> Result<RawTelemetryEnvelope, String> {
            if self.0.borrow().silent {
                return Err("no publication yet".to_owned());
            }
            let mut envelope = RawTelemetryEnvelope::new(
                "guest",
                "RampireGuest",
                "session",
                now(),
                1,
                0,
                MemoryTelemetrySnapshot {
                    physical_total_bytes: 8 * GIB,
                    physical_available_bytes: GIB,
                    memory_load_percent: 80,
                    commit_total_bytes: 4 * GIB,
                    commit_limit_bytes: 8 * GIB,
                    commit_peak_bytes: 4 * GIB,
                    system_cache_bytes: 0,
                    kernel_paged_bytes: 0,
                    kernel_nonpaged_bytes: 0,
                },
            );
            envelope.request = self.0.borrow().request.clone();
            envelope.native_pressure = self.0.borrow().native;
            if let Some(device) = &self.0.borrow().device {
                envelope.memory.physical_total_bytes = device.live().current_bytes;
            }
            Ok(envelope)
        }

        fn publish_status(&self, status: &HostMemoryStatus) -> Result<(), String> {
            status.validate_for("guest", "RampireGuest")?;
            self.0.borrow_mut().statuses.push(status.clone());
            Ok(())
        }
    }

    struct HostAvailable(u64);

    impl HostMemorySource for HostAvailable {
        fn available_bytes(&self) -> Result<u64, String> {
            Ok(self.0)
        }
    }

    fn now() -> u64 {
        SystemUnixClock.now_unix_millis().unwrap()
    }

    fn request(sequence: u64, operation: MemoryOperation, bytes: u64) -> MemoryRequest {
        let issued = now() - 1000 + sequence;
        MemoryRequest {
            version: MEMORY_REQUEST_VERSION,
            identity: RequestIdentity {
                vm_name: "guest".to_owned(),
                service_name: "RampireGuest".to_owned(),
                session_id: "session".to_owned(),
                sequence,
            },
            operation,
            bytes,
            issued_unix_millis: issued,
            expires_unix_millis: issued + 60_000,
        }
    }

    fn config(test: &str, priority: u32) -> HostConfig {
        let path = std::env::temp_dir().join(format!(
            "rampire-runtime-{test}-{priority}-{}.json",
            std::process::id()
        ));
        // A previous failed run may have left state behind.
        let _ = std::fs::remove_file(&path);
        HostConfig {
            vm_name: "guest".to_owned(),
            alias: "memory0".to_owned(),
            priority,
            policy_state_path: path.display().to_string(),
            poll_interval: Duration::from_millis(1),
            command_timeout: Duration::from_secs(10),
            convergence_timeout: Duration::from_secs(300),
            removal_settle: Duration::from_secs(300),
            virsh_binary: "virsh".to_owned(),
            libvirt_uri: "qemu:///system".to_owned(),
            raw_telemetry_path: "telemetry.json".to_owned(),
            raw_telemetry_service_name: "RampireGuest".to_owned(),
            guest_os: GuestOs::Windows,
            raw_telemetry_max_age: Duration::from_secs(60),
            raw_telemetry_future_tolerance: Duration::from_secs(5),
            host_min_headroom_bytes: 4 * GIB,
            compatibility_attestation_path: "attestation.json".to_owned(),
            observe_only: false,
            min_ram: 0,
            max_ram: u64::MAX,
            guest_settings: None,
            telemetry_path: None,
        }
    }

    struct Fixture {
        guest: Guest,
        device: Device,
        targets: Rc<RefCell<Vec<u64>>>,
        config: HostConfig,
    }

    impl Fixture {
        fn new(test: &str, priority: u32, current_bytes: u64) -> Self {
            Self {
                guest: Guest::default(),
                device: Device::new(current_bytes),
                targets: Rc::default(),
                config: config(test, priority),
            }
        }

        fn controller(
            &self,
            failure: Option<ResizeSinkError>,
        ) -> VmController<Guest, Device, Sink> {
            VmController::new(
                self.guest.clone(),
                self.device.clone(),
                Sink {
                    device: self.device.clone(),
                    targets: Rc::clone(&self.targets),
                    failure,
                },
                self.config.clone(),
                "a".repeat(64),
            )
            .expect("controller")
        }

        fn journal(&self) -> DurableControlState {
            let (_, control) =
                Checkpoint::open(&self.config, "a".repeat(64)).expect("reopen checkpoint");
            control
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            // Test cleanup: a missing file is fine.
            let _ = std::fs::remove_file(&self.config.policy_state_path);
        }
    }

    type Runtime = HostRuntime<Guest, Device, Sink, HostAvailable>;

    fn runtime(
        vms: Vec<VmController<Guest, Device, Sink>>,
        available: u64,
        pool: Option<u64>,
    ) -> Runtime {
        HostRuntime::new(vms, HostAvailable(available), pool).expect("runtime")
    }

    fn round(runtime: &mut Runtime) -> Step {
        runtime.round(&AtomicBool::new(false)).expect("round")
    }

    #[test]
    fn host_enforces_vm_ram_bounds_without_optional_telemetry() {
        for (name, operation, minimum, maximum, expected) in [
            ("ram-max", MemoryOperation::Add, 0, 9 * GIB, 5 * GIB),
            (
                "ram-min",
                MemoryOperation::Remove,
                7 * GIB,
                20 * GIB,
                3 * GIB,
            ),
        ] {
            let mut fixture = Fixture::new(name, 100, 4 * GIB);
            fixture.config.min_ram = minimum;
            fixture.config.max_ram = maximum;
            fixture.guest.send(Some(request(1, operation, 4 * GIB)));
            let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
            round(&mut runtime);
            assert_eq!(*fixture.targets.borrow(), [expected]);
        }
    }

    #[test]
    fn guest_settings_reach_every_guest() {
        let settings = rampire_core::settings::GlobalSettings::default().guest_policy(GIB, 8 * GIB);
        let published = rampire_core::GuestSettings {
            policy: settings,
            min_ram: 4 * GIB,
            max_ram: 16 * GIB,
            poll_interval_millis: 5000,
        };
        let sent = |silent: bool| {
            let mut fixture = Fixture::new("guest-settings", 100, 4 * GIB);
            fixture.config.guest_settings = Some(published);
            {
                let mut guest = fixture.guest.0.borrow_mut();
                guest.silent = silent;
            }
            let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
            round(&mut runtime);
            let statuses = &fixture.guest.0.borrow().statuses;
            statuses.last().expect("a status").guest_settings
        };
        // A guest still waiting for its settings, and one publishing, get them.
        assert_eq!(sent(true), Some(published));
        assert_eq!(sent(false), Some(published));
    }

    #[test]
    fn optional_diagnostic_failure_does_not_block_memory_control() {
        let mut fixture = Fixture::new("optional-diagnostic", 100, 4 * GIB);
        // A checkpoint file is a regular file, so it cannot be a directory.
        fixture.config.telemetry_path = Some(format!(
            "{}/diagnostic.json",
            fixture.config.policy_state_path
        ));
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Add, GIB)));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
    }

    #[test]
    fn returns_immediately_for_a_cancelled_runtime() {
        let fixture = Fixture::new("cancelled", 100, 4 * GIB);
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        let stop = AtomicBool::new(true);
        assert!(runtime.run(&stop).is_ok());
        assert!(fixture.guest.0.borrow().statuses.is_empty());
    }

    #[test]
    fn an_add_is_executed_once_and_resolved_with_its_actual_bytes() {
        let fixture = Fixture::new("add", 100, 4 * GIB);
        let add = request(1, MemoryOperation::Add, GIB);
        fixture.guest.send(Some(add.clone()));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);

        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
        assert_eq!(fixture.guest.last_result(), None, "executing");

        // Convergence resolves the request; retransmission only republishes.
        for _ in 0..3 {
            round(&mut runtime);
            let result = fixture.guest.last_result().expect("result");
            assert_eq!(result.request, add);
            assert_eq!(result.outcome, MemoryOutcome::Completed);
            assert_eq!(result.actual_bytes, GIB);
        }
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
        drop(runtime);
        let control = fixture.journal();
        assert!(control.intent.is_none() && !control.latched());
        assert_eq!(
            control.journal.result.map(|result| result.actual_bytes),
            Some(GIB)
        );
    }

    #[test]
    fn the_host_reserve_limits_a_grant_and_no_remainder_is_queued() {
        let fixture = Fixture::new("reserve", 100, 4 * GIB);
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Add, 4 * GIB)));
        // Admission retains 256 MiB above the hard 4 GiB reserve.
        let mut runtime = runtime(vec![fixture.controller(None)], 6 * GIB, None);
        round(&mut runtime);
        round(&mut runtime);
        let partial = fixture.guest.last_result().expect("result");
        assert_eq!(partial.outcome, MemoryOutcome::Partial);
        assert_eq!(partial.actual_bytes, 2 * GIB - 256 * MIB);
        round(&mut runtime);
        assert_eq!(
            *fixture.targets.borrow(),
            [6 * GIB - 256 * MIB],
            "no remainder queue"
        );
    }

    #[test]
    fn strict_priority_grants_free_pool_bytes_and_reports_an_empty_pool() {
        let high = Fixture::new("priority", 200, 4 * GIB);
        let low = Fixture::new("priority", 100, 4 * GIB);
        for fixture in [&high, &low] {
            fixture
                .guest
                .send(Some(request(1, MemoryOperation::Add, 2 * GIB)));
        }
        // Configuration order puts the low-priority VM first.
        let mut runtime = runtime(
            vec![low.controller(None), high.controller(None)],
            64 * GIB,
            Some(10 * GIB),
        );
        round(&mut runtime);
        assert_eq!(*high.targets.borrow(), [6 * GIB]);
        assert!(low.targets.borrow().is_empty());
        let empty = low.guest.last_result().expect("pool-empty result");
        assert_eq!(empty.outcome, MemoryOutcome::PoolEmpty);
        assert_eq!(empty.actual_bytes, 0);
        drop(runtime);
        assert!(low.journal().journal.result.is_some(), "journalled");
    }

    #[test]
    fn equal_priorities_are_granted_first_come_first_served() {
        let listed_first = Fixture::new("fcfs-first", 100, 4 * GIB);
        let asked_first = Fixture::new("fcfs-second", 100, 4 * GIB);
        // The VM listed second asked a millisecond earlier.
        listed_first
            .guest
            .send(Some(request(2, MemoryOperation::Add, 2 * GIB)));
        asked_first
            .guest
            .send(Some(request(1, MemoryOperation::Add, 2 * GIB)));
        let mut runtime = runtime(
            vec![listed_first.controller(None), asked_first.controller(None)],
            64 * GIB,
            Some(10 * GIB),
        );
        round(&mut runtime);
        assert_eq!(*asked_first.targets.borrow(), [6 * GIB]);
        assert!(listed_first.targets.borrow().is_empty());
        let empty = listed_first.guest.last_result().expect("pool-empty result");
        assert_eq!(empty.outcome, MemoryOutcome::PoolEmpty);
    }

    #[test]
    fn refusals_before_any_command_are_published_without_latching() {
        let fixture = Fixture::new("observe-only", 100, 4 * GIB);
        let mut observe = fixture.config.clone();
        observe.observe_only = true;
        let add = request(1, MemoryOperation::Add, GIB);
        fixture.guest.send(Some(add.clone()));
        {
            let mut runtime = runtime(
                vec![VmController::new(
                    fixture.guest.clone(),
                    fixture.device.clone(),
                    Sink {
                        device: fixture.device.clone(),
                        targets: Rc::clone(&fixture.targets),
                        failure: None,
                    },
                    observe,
                    "a".repeat(64),
                )
                .expect("controller")],
                64 * GIB,
                None,
            );
            round(&mut runtime);
        }
        let refused = fixture.guest.last_result().expect("refusal");
        assert_eq!(refused.outcome, MemoryOutcome::Rejected);
        assert!(refused
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("would grant 1073741824")));
        assert!(fixture.targets.borrow().is_empty());
        assert_eq!(fixture.journal(), DurableControlState::default());

        // A command refused at its final check releases the intent unlatched.
        let mut runtime = runtime(
            vec![fixture.controller(Some(ResizeSinkError::Rejected(
                "attestation drifted".to_owned(),
            )))],
            64 * GIB,
            None,
        );
        round(&mut runtime);
        drop(runtime);
        let control = fixture.journal();
        assert!(!control.latched() && control.intent.is_none());
        assert_eq!(
            control.journal.result.map(|result| result.outcome),
            Some(MemoryOutcome::Rejected)
        );
    }

    #[test]
    fn stale_or_superseded_requests_are_refused_without_admission() {
        let fixture = Fixture::new("stale", 100, 4 * GIB);
        let mut expired = request(1, MemoryOperation::Add, GIB);
        expired.issued_unix_millis = now() - 120_000;
        expired.expires_unix_millis = expired.issued_unix_millis + 60_000;
        fixture.guest.send(Some(expired));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        assert_eq!(
            fixture.guest.last_result().map(|result| result.outcome),
            Some(MemoryOutcome::Rejected)
        );

        fixture
            .guest
            .send(Some(request(5, MemoryOperation::Add, GIB)));
        round(&mut runtime);
        round(&mut runtime);
        // An older identity after a newer one is a replay, not a new delta.
        fixture
            .guest
            .send(Some(request(4, MemoryOperation::Add, GIB)));
        round(&mut runtime);
        assert_eq!(
            fixture.guest.last_result().map(|result| result.outcome),
            Some(MemoryOutcome::Rejected)
        );
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
    }

    #[test]
    fn an_unknown_growth_outcome_is_uncertain_latched_and_never_replayed() {
        let fixture = Fixture::new("unknown", 100, 4 * GIB);
        let add = request(1, MemoryOperation::Add, GIB);
        fixture.guest.send(Some(add.clone()));
        {
            let mut runtime = runtime(
                vec![fixture.controller(Some(ResizeSinkError::CommandUnknown(
                    "virsh timed out".to_owned(),
                )))],
                64 * GIB,
                None,
            );
            assert!(matches!(round(&mut runtime), Step::Stop(_)));
        }
        assert_eq!(
            fixture.guest.last_result().map(|result| result.outcome),
            Some(MemoryOutcome::Uncertain)
        );
        assert!(fixture.journal().latched());

        // After restart the effect is observed, not replayed.
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        let observed = fixture.guest.last_result().expect("result");
        assert_eq!(observed.outcome, MemoryOutcome::Completed);
        fixture
            .guest
            .send(Some(request(2, MemoryOperation::Add, GIB)));
        round(&mut runtime);
        assert_eq!(
            fixture.guest.last_result().map(|result| result.outcome),
            Some(MemoryOutcome::Rejected),
            "latched actuation refuses new requests"
        );
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
    }

    #[test]
    fn a_removal_stalled_at_the_floor_settles_without_losing_its_result() {
        let mut fixture = Fixture::new("floor-shrink", 100, 8 * GIB);
        fixture.config.removal_settle = Duration::from_millis(20);
        fixture.guest.track(&fixture.device);
        fixture.config.min_ram = 7 * GIB;
        fixture.device.0.borrow_mut().stall_at = Some(7 * GIB + 512 * MIB);
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Remove, GIB)));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [7 * GIB]);
        assert!(fixture.guest.last_result().is_none());
        thread::sleep(Duration::from_millis(30));
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [7 * GIB, 7 * GIB + 512 * MIB]);
        let result = fixture.guest.last_result().expect("partial result");
        assert_eq!(result.outcome, MemoryOutcome::Partial);
        assert_eq!(result.actual_bytes, 512 * MIB);
        // Pressure can now issue a fresh addition rather than remaining blocked.
        fixture
            .guest
            .send(Some(request(2, MemoryOperation::Add, GIB)));
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(
            fixture.guest.last_result().unwrap().outcome,
            MemoryOutcome::Completed
        );
        drop(runtime);
        assert!(!fixture.journal().latched());
    }

    #[test]
    fn a_held_removal_is_released_when_windows_reports_low_memory() {
        let fixture = Fixture::new("low-release", 100, 8 * GIB);
        fixture.device.0.borrow_mut().stall_at = Some(7 * GIB + 512 * MIB);
        fixture.guest.signal(MemoryResourceNotificationState::High);
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Remove, GIB)));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [7 * GIB]);

        // Windows runs short: settle at what it reached, report partial.
        fixture.guest.signal(MemoryResourceNotificationState::Low);
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [7 * GIB, 7 * GIB + 512 * MIB]);
        let result = fixture.guest.last_result().expect("result");
        assert_eq!(result.outcome, MemoryOutcome::Partial);
        assert_eq!(result.actual_bytes, 512 * MIB);
        drop(runtime);
        assert!(!fixture.journal().latched());
    }

    #[test]
    fn a_removal_is_never_extended_and_settles_once_it_stops_moving() {
        for guest_os in [GuestOs::Linux, GuestOs::Windows] {
            removal_settles_once_it_stops_moving(guest_os);
        }
    }

    // PAUSED (viomem): Windows used to extend a stalled shrink; with the
    // workaround archived it settles like Linux.
    fn removal_settles_once_it_stops_moving(guest_os: GuestOs) {
        let mut fixture = Fixture::new(&format!("shrink-{guest_os:?}"), 100, 8 * GIB);
        fixture.config.guest_os = guest_os;
        fixture.config.poll_interval = Duration::from_millis(2);
        fixture.config.removal_settle = Duration::from_millis(300);
        fixture.device.0.borrow_mut().stall_at = Some(7 * GIB + 512 * MIB);
        fixture.guest.signal(MemoryResourceNotificationState::High);
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Remove, GIB)));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        for _ in 0..4 {
            round(&mut runtime);
            thread::sleep(Duration::from_millis(5));
        }
        // Stalled for several polls: the target is held, not lowered.
        assert_eq!(*fixture.targets.borrow(), [7 * GIB]);
        assert!(fixture.guest.last_result().is_none());

        thread::sleep(Duration::from_millis(350));
        round(&mut runtime);
        round(&mut runtime);
        assert_eq!(*fixture.targets.borrow(), [7 * GIB, 7 * GIB + 512 * MIB]);
        let result = fixture.guest.last_result().expect("result");
        assert_eq!(result.outcome, MemoryOutcome::Partial);
        assert_eq!(result.actual_bytes, 512 * MIB);
        drop(runtime);
        assert!(!fixture.journal().latched());
    }

    #[test]
    fn a_partly_plugged_growth_holds_its_target() {
        let mut fixture = Fixture::new("hold-grow", 100, 4 * GIB);
        fixture.config.poll_interval = Duration::from_millis(2);
        fixture.config.convergence_timeout = Duration::from_millis(1);
        fixture.device.0.borrow_mut().grow_stall_at = Some(4 * GIB + 512 * MIB);
        fixture
            .guest
            .send(Some(request(1, MemoryOperation::Add, GIB)));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        for _ in 0..4 {
            round(&mut runtime);
            thread::sleep(Duration::from_millis(5));
        }
        // No settle and no widening: only the original target was set.
        assert_eq!(*fixture.targets.borrow(), [5 * GIB]);
        assert!(fixture.guest.last_result().is_none());
        drop(runtime);
        assert!(!fixture.journal().latched());
    }

    #[test]
    fn restart_latches_a_recorded_command_that_never_applied() {
        let fixture = Fixture::new("not-applied", 100, 4 * GIB);
        let add = request(1, MemoryOperation::Add, GIB);
        {
            let (checkpoint, mut control) =
                Checkpoint::open(&fixture.config, "a".repeat(64)).expect("checkpoint");
            control
                .journal
                .admit(add.clone(), "guest", "RampireGuest", now(), 0)
                .expect("admit");
            control.intent = Some(CommandIntent {
                operation_id: "earlier".to_owned(),
                started_unix_millis: now(),
                direction: CommandDirection::Grow,
                prior_requested_bytes: 4 * GIB,
                prior_current_bytes: 4 * GIB,
                target_bytes: 5 * GIB,
            });
            checkpoint.save(&control).expect("save");
        }
        fixture.guest.send(Some(add));
        let mut runtime = runtime(vec![fixture.controller(None)], 64 * GIB, None);
        round(&mut runtime);
        assert!(fixture.targets.borrow().is_empty());
        assert_eq!(
            fixture.guest.last_result().map(|result| result.outcome),
            Some(MemoryOutcome::Uncertain)
        );
        drop(runtime);
        assert!(fixture.journal().latched());
    }
}
