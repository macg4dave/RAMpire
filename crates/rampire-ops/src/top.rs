//! What `rampire top` and the TUI's dashboard read about the whole pool:
//! each VM's device and guest, the host's memory, a history of the pool's
//! charge and the controller's pool events for all VMs. The drawing is in
//! [`crate::tui::pool`].
//!
//! It reads `virsh --readonly dumpxml` per VM, the controller's copy of each
//! guest's publication (`/run/rampire/<vm>.publication.json`, so no
//! guest-agent traffic) and `journalctl`. It never resizes or changes
//! anything.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rampire_core::{RawTelemetryEnvelope, VirtioMemState};
use rampire_host::config::HostSettings;
use rampire_host::guest_channel::publication_copy_path;
use rampire_host::virsh::Virsh;

use crate::dashboard::{mib, GIB};
use crate::host_readings::{
    device_state, read_meminfo, HostMemory, COMMAND_TIMEOUT, MAX_TELEMETRY_BYTES,
};
use crate::journal::{self, field};
use crate::process;
use crate::tone::{self, Tone};

pub const HELP: &str = "Pool options (read-only live dashboard of every VM):
  --config PATH                 Required host list; the VMs, pool size and
                                host floor come from it.
  --connect URI                 Libvirt URI; default qemu:///system.
  --interval-seconds N          Refresh period; default 1.
  --events N                    Pool events shown; default 8.
  --once                        Print one plain snapshot.
Ctrl+C or q leaves a plain snapshot on the screen to copy.
";

/// Samples kept for the chart: more than the widest terminal shows.
pub const HISTORY_SAMPLES: usize = 300;

/// A publication older than this is shown as stale.
pub const STALE_PUBLICATION: Duration = Duration::from_secs(30);

/// A managed VM as the pool sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vm {
    pub name: String,
    pub alias: String,
    pub priority: u32,
    pub min_ram: u64,
    pub max_ram: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub vms: Vec<Vm>,
    /// `None` for a single-VM controller, which has no shared pool.
    pub pool_bytes: Option<u64>,
    pub host_floor_bytes: u64,
    pub unit: String,
    pub connect: String,
    pub interval: Duration,
    pub events: usize,
    pub once: bool,
}

impl Command {
    /// The pool of the configuration in `settings`, which may manage no VMs.
    pub fn from_settings(settings: &HostSettings, connect: &str) -> Self {
        Self {
            vms: settings
                .vms
                .iter()
                .map(|vm| Vm {
                    name: vm.vm_name.clone(),
                    alias: vm.alias.clone(),
                    priority: vm.priority,
                    min_ram: vm.min_ram,
                    max_ram: vm.max_ram,
                })
                .collect(),
            pool_bytes: settings.pool_bytes,
            host_floor_bytes: settings
                .vms
                .iter()
                .map(|vm| vm.host_min_headroom_bytes)
                .max()
                .unwrap_or(0),
            unit: crate::systemd_unit::CONTROLLER_UNIT.to_owned(),
            connect: connect.to_owned(),
            interval: Duration::from_secs(1),
            events: 8,
            once: false,
        }
    }
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut config_path = None;
    let mut connect = "qemu:///system".to_owned();
    let mut interval = Duration::from_secs(1);
    let mut events = 8;
    let mut once = false;
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        if option == "--once" {
            once = true;
            index += 1;
            continue;
        }
        let value = args
            .get(index + 1)
            .ok_or_else(|| format!("{option} requires a value"))?;
        match option {
            "--config" => config_path = Some(PathBuf::from(value)),
            "--connect" => connect = value.clone(),
            "--interval-seconds" => {
                interval = value
                    .parse::<f64>()
                    .ok()
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.1)
                    .map(Duration::from_secs_f64)
                    .ok_or("--interval-seconds requires a number of seconds >= 0.1")?;
            }
            "--events" => {
                events = value
                    .parse()
                    .ok()
                    .filter(|count| (1..=30).contains(count))
                    .ok_or("--events requires 1..=30")?;
            }
            _ => return Err(format!("unknown pool option: {option}\n\n{HELP}")),
        }
        index += 2;
    }
    let config_path = config_path.ok_or_else(|| format!("pool requires --config\n\n{HELP}"))?;
    let settings = HostSettings::load(&config_path)
        .map_err(|error| format!("{}: {error}", config_path.display()))?;
    let mut command = Command::from_settings(&settings, &connect);
    if command.vms.is_empty() {
        return Err("RAMpire manages no VMs yet: there is no pool to show".to_owned());
    }
    command.interval = interval;
    command.events = events;
    command.once = once;
    Ok(command)
}

/// One VM's device and guest at one refresh.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VmSample {
    pub device: Option<VirtioMemState>,
    pub guest: Option<RawTelemetryEnvelope>,
}

impl VmSample {
    /// What the pool charges this VM: `max(requested, plugged)`, as the
    /// controller counts it; a recorded command libvirt doesn't show yet is
    /// not visible here.
    pub fn charge(&self) -> u64 {
        self.device.map_or(0, |device| device.pool_charge(0))
    }
}

/// Session extremes, so a brief squeeze stays visible after it passes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Session {
    pub peak_charged: u64,
    pub low_free: Option<u64>,
    pub low_host_available: Option<u64>,
}

impl Session {
    fn record(&mut self, charged: u64, free: Option<u64>, host_available: u64) {
        self.peak_charged = self.peak_charged.max(charged);
        if let Some(free) = free {
            self.low_free = Some(self.low_free.map_or(free, |low| low.min(free)));
        }
        self.low_host_available = Some(
            self.low_host_available
                .map_or(host_available, |low| low.min(host_available)),
        );
    }
}

/// Everything one refresh of the pool shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PoolSnapshot {
    pub command: Command,
    pub host: HostMemory,
    /// In the order of `command.vms`.
    pub vms: Vec<VmSample>,
    pub now_unix_millis: u64,
    pub samples: u64,
    pub session: Session,
    /// Each VM's charge at each refresh, oldest first.
    pub history: Vec<Vec<u64>>,
    /// The host's available memory at each refresh, beside `history`.
    pub host_history: Vec<u64>,
    /// Newest first, or why the journal gave none.
    pub events: Result<Vec<PoolEvent>, String>,
}

impl PoolSnapshot {
    pub fn charges(&self) -> Vec<u64> {
        self.vms.iter().map(VmSample::charge).collect()
    }

    pub fn charged(&self) -> u64 {
        self.vms
            .iter()
            .map(VmSample::charge)
            .fold(0, u64::saturating_add)
    }

    /// The pool's free memory, with a pool.
    pub fn free(&self) -> Option<u64> {
        self.command
            .pool_bytes
            .map(|pool| pool.saturating_sub(self.charged()))
    }

    /// The bar's full scale: the pool, or with no pool the larger of what is
    /// charged and every device's size.
    pub fn scale(&self) -> u64 {
        self.command.pool_bytes.unwrap_or_else(|| {
            self.vms
                .iter()
                .map(|vm| vm.device.map_or(0, |device| device.size_bytes))
                .fold(0, u64::saturating_add)
                .max(self.charged())
        })
    }

    /// Resizes under way: how many VMs, and the bytes still to plug and to
    /// unplug.
    pub fn resizing(&self) -> (usize, u64, u64) {
        self.vms
            .iter()
            .filter_map(|vm| vm.device)
            .filter(|device| device.requested_bytes != device.current_bytes)
            .fold((0, 0, 0), |(count, growing, shrinking), device| {
                (
                    count + 1,
                    growing + device.requested_bytes.saturating_sub(device.current_bytes),
                    shrinking + device.current_bytes.saturating_sub(device.requested_bytes),
                )
            })
    }
}

/// Reads the pool at each refresh, keeping its history and session
/// extremes between refreshes.
pub struct PoolReader {
    command: Command,
    virsh: Virsh,
    history: VecDeque<Vec<u64>>,
    host_history: VecDeque<u64>,
    session: Session,
    samples: u64,
}

impl PoolReader {
    pub fn new(command: Command) -> Self {
        Self {
            virsh: Virsh::persistent_with_connection("virsh", COMMAND_TIMEOUT, &command.connect),
            command,
            history: VecDeque::new(),
            host_history: VecDeque::new(),
            session: Session::default(),
            samples: 0,
        }
    }

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn read(&mut self, cwd: &Path) -> Result<PoolSnapshot, String> {
        self.samples += 1;
        let vms: Vec<VmSample> = self
            .command
            .vms
            .iter()
            .map(|vm| VmSample {
                device: device_state(&self.virsh, &vm.name, &vm.alias),
                guest: read_publication(&publication_copy_path(&vm.name)),
            })
            .collect();
        let host = read_meminfo()?;
        let charged = vms
            .iter()
            .map(VmSample::charge)
            .fold(0, u64::saturating_add);
        self.session.record(
            charged,
            self.command
                .pool_bytes
                .map(|pool| pool.saturating_sub(charged)),
            host.available,
        );
        self.history
            .push_back(vms.iter().map(VmSample::charge).collect());
        self.host_history.push_back(host.available);
        if self.history.len() > HISTORY_SAMPLES {
            self.history.pop_front();
            self.host_history.pop_front();
        }
        Ok(PoolSnapshot {
            command: self.command.clone(),
            host,
            vms,
            now_unix_millis: process::unix_millis()?,
            samples: self.samples,
            session: self.session,
            history: self.history.iter().cloned().collect(),
            host_history: self.host_history.iter().copied().collect(),
            events: read_events(&self.command, cwd),
        })
    }
}

/// The controller's copy of a guest's publication; missing (controller
/// stopped), oversized or mid-write leaves it empty.
pub fn read_publication(path: &Path) -> Option<RawTelemetryEnvelope> {
    let bytes = rampire_core::files::read_bounded(path, MAX_TELEMETRY_BYTES).ok()??;
    serde_json::from_slice(&bytes).ok()
}

/// One pool event, short.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEvent {
    pub time: String,
    pub vm: String,
    pub text: String,
    pub tone: Tone,
}

/// The pool events, or why the journal gave none.
fn read_events(command: &Command, cwd: &Path) -> Result<Vec<PoolEvent>, String> {
    journal::read_events(&command.unit, cwd).map(|text| parse_events(&text, command.events))
}

fn mebibytes(line: &str, name: &str) -> String {
    field(line, name)
        .and_then(|value| value.parse::<u64>().ok())
        .map_or_else(|| "?".to_owned(), |bytes| format!("{}M", mib(bytes)))
}

/// One pool-relevant journal line as a short event with its tone.
fn pool_event(line: &str) -> Option<PoolEvent> {
    let event = field(line, "event")?;
    let operation = || field(line, "operation").unwrap_or("?");
    let (text, tone) = match event {
        "request_completed" => {
            let outcome = field(line, "outcome").unwrap_or("?");
            let mut text = format!(
                "{} {} -> {} {outcome}",
                operation(),
                mebibytes(line, "bytes"),
                mebibytes(line, "actual_bytes")
            );
            if let Some(reason) = field(line, "reason") {
                text.push_str(&format!(": {reason}"));
            }
            (text, tone::outcome(outcome))
        }
        "controller_decision" => match field(line, "action")? {
            "grant" => (
                format!(
                    "grant {} {} of {}",
                    operation(),
                    mebibytes(line, "grant_bytes"),
                    mebibytes(line, "bytes")
                ),
                Tone::Info,
            ),
            "refuse" => (
                format!(
                    "refused {} {}: {}",
                    operation(),
                    mebibytes(line, "bytes"),
                    field(line, "reason").unwrap_or("?")
                ),
                Tone::Bad,
            ),
            _ => return None,
        },
        "resize_settled_short" => (
            format!(
                "settled short at {} of {}",
                mebibytes(line, "kept_bytes"),
                mebibytes(line, "target_bytes")
            ),
            Tone::Warn,
        ),
        "actuation_latched" => (
            format!("LATCHED: {}", field(line, "reason").unwrap_or("?")),
            Tone::Bad,
        ),
        "vm_stopped" => (
            format!("stopped: {}", field(line, "reason").unwrap_or("?")),
            Tone::Bad,
        ),
        "control_channel_lost" => ("control channel lost".to_owned(), Tone::Warn),
        "control_channel_connected" => ("control channel connected".to_owned(), Tone::Dim),
        _ => return None,
    };
    Some(PoolEvent {
        time: journal::time(line).to_owned(),
        vm: field(line, "vm").unwrap_or("?").to_owned(),
        text,
        tone,
    })
}

/// Newest-first pool events, consecutive repeats per VM collapsed with a count.
pub fn parse_events(journal: &str, count: usize) -> Vec<PoolEvent> {
    journal::collapse_repeats(
        journal.lines().rev().filter_map(pool_event),
        count,
        |newer, older| newer.vm == older.vm && newer.text == older.text,
    )
    .into_iter()
    .map(|(mut event, repeats)| {
        if repeats > 1 {
            event.text = format!("(x{repeats}) {}", event.text);
        }
        event
    })
    .collect()
}

/// GiB with two decimals and a `G`, for the narrow table columns.
pub fn short_gib(bytes: u64) -> String {
    format!("{:.2}G", bytes as f64 / GIB as f64)
}

/// Where each VM's segment ends in a bar of `cells` scaled to `scale`
/// bytes: cumulative rounding, so segments never overlap or overflow.
pub fn segment_ends(charges: &[u64], scale: u64, cells: usize) -> Vec<usize> {
    let scale = scale.max(1) as f64;
    let mut total = 0u64;
    charges
        .iter()
        .map(|charge| {
            total = total.saturating_add(*charge);
            ((total as f64 / scale * cells as f64).round() as usize).min(cells)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/config")
            .to_string_lossy()
            .into_owned()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn reads_every_vm_and_the_pool_from_the_configuration() {
        let config = fixture();
        let settings = HostSettings::load(Path::new(&config)).expect("tracked config");
        let command = parse(&strings(&["--config", &config])).expect("defaults");
        assert_eq!(command.vms.len(), settings.vms.len());
        assert_eq!(command.vms[0].name, settings.vms[0].vm_name);
        assert_eq!(command.pool_bytes, settings.pool_bytes);
        assert_eq!(command.unit, "rampire.service");
        let command = parse(&strings(&[
            "--config",
            &config,
            "--interval-seconds",
            "0.5",
            "--events",
            "3",
            "--once",
        ]))
        .expect("options");
        assert!(command.once);
        assert_eq!(command.interval, Duration::from_millis(500));
        assert_eq!(command.events, 3);
        for bad in [
            vec![],
            vec!["--config"],
            vec!["--config", &config, "--interval-seconds", "0"],
            vec!["--config", &config, "--events", "0"],
            vec!["--config", &config, "--vm", "x"],
        ] {
            assert!(parse(&strings(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn segments_stack_without_overlap_or_overflow() {
        assert_eq!(segment_ends(&[GIB, GIB, 2 * GIB], 4 * GIB, 8), [2, 4, 8]);
        assert_eq!(segment_ends(&[0, GIB], 4 * GIB, 8), [0, 2]);
        // Over the scale (a shrink still charged) clips at the bar's end.
        assert_eq!(segment_ends(&[3 * GIB, 3 * GIB], 4 * GIB, 8), [6, 8]);
        assert_eq!(segment_ends(&[GIB], 0, 8), [8]);
    }

    #[test]
    fn pool_events_cover_every_vm_newest_first() {
        let journal = "\
2026-11-13T10:00:00+00:00 h rampire[1]: rampire: vm=alpha alias=m0 event=control_health direction=Grow
2026-11-13T10:00:01+00:00 h rampire[1]: rampire: vm=alpha alias=m0 event=controller_decision action=grant session_id=s sequence=1 operation=Add bytes=1073741824 grant_bytes=536870912 pool_ceiling_bytes=None requested_bytes=0 current_bytes=0
2026-11-13T10:00:02+00:00 h rampire[1]: rampire: vm=beta alias=m0 event=request_completed operation_id=none session_id=s sequence=4 operation=Add bytes=1073741824 outcome=PoolEmpty actual_bytes=0 reason=the pool is fully charged
2026-11-13T10:00:03+00:00 h rampire[1]: rampire: vm=beta alias=m0 event=request_completed operation_id=none session_id=s sequence=4 operation=Add bytes=1073741824 outcome=PoolEmpty actual_bytes=0 reason=the pool is fully charged
2026-11-13T10:00:04+00:00 h rampire[1]: rampire: vm=alpha alias=m0 event=request_completed operation_id=7 session_id=s sequence=1 operation=Add bytes=1073741824 outcome=Partial actual_bytes=536870912
";
        let events = parse_events(journal, 8);
        assert_eq!(
            events,
            [
                PoolEvent {
                    time: "10:00:04".to_owned(),
                    vm: "alpha".to_owned(),
                    text: "Add 1024M -> 512M Partial".to_owned(),
                    tone: Tone::Warn,
                },
                PoolEvent {
                    time: "10:00:03".to_owned(),
                    vm: "beta".to_owned(),
                    text: "(x2) Add 1024M -> 0M PoolEmpty: the pool is fully charged".to_owned(),
                    tone: Tone::Warn,
                },
                PoolEvent {
                    time: "10:00:01".to_owned(),
                    vm: "alpha".to_owned(),
                    text: "grant Add 512M of 1024M".to_owned(),
                    tone: Tone::Info,
                },
            ]
        );
        assert_eq!(parse_events(journal, 1).len(), 1);
        assert!(parse_events("no events here", 8).is_empty());
    }

    #[test]
    fn a_snapshot_sums_charges_free_memory_and_resizes() {
        let config = fixture();
        let mut command = parse(&strings(&["--config", &config])).unwrap();
        command.pool_bytes = Some(8 * GIB);
        let device = |requested: u64, current: u64| {
            Some(VirtioMemState {
                size_bytes: 8 * GIB,
                block_size_bytes: 2 << 20,
                requested_bytes: requested,
                current_bytes: current,
            })
        };
        let snapshot = PoolSnapshot {
            command,
            host: HostMemory::default(),
            vms: vec![
                VmSample {
                    device: device(2 * GIB, GIB),
                    guest: None,
                },
                VmSample {
                    device: None,
                    guest: None,
                },
            ],
            now_unix_millis: 0,
            samples: 1,
            session: Session::default(),
            history: vec![],
            host_history: vec![],
            events: Ok(vec![]),
        };
        assert_eq!(snapshot.charged(), 2 * GIB);
        assert_eq!(snapshot.free(), Some(6 * GIB));
        assert_eq!(snapshot.resizing(), (1, GIB, 0));
        assert_eq!(snapshot.scale(), 8 * GIB);
    }
}
