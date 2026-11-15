//! What `rampire watch` and the TUI's VM details read about one VM: host
//! RAM against the controller floor, the device, the guest's own view from
//! its latest publication, a device history and the controller's latest
//! journal events. The drawing is in [`crate::tui::vm`].
//!
//! It uses `virsh --readonly dumpxml`, the guest's publication (the
//! controller's copy, or a guest-agent read without it) and `journalctl`. It
//! never resizes, clears latches or restarts anything. It deliberately does
//! not read `.host-status`: a guest-agent read holds that file open across
//! several commands and makes the controller's write of it fail with a
//! sharing violation; the controller journal records the same results.

use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

use rampire_core::{MemoryResourceNotificationState, RawTelemetryEnvelope, VirtioMemState};
use rampire_host::config::HostConfig;
use rampire_host::guest_channel::{PublicationCopyFirst, PublicationSource};
use rampire_host::qga::{GuestFileReader, VirshGuestFiles};
use rampire_host::virsh::Virsh;

use crate::dashboard::mib;
use crate::host_readings::{
    device_state, read_meminfo, HostMemory, COMMAND_TIMEOUT, MAX_TELEMETRY_BYTES,
};
use crate::journal::{self, field};
use crate::tone::{self, Tone};
use rampire_core::MIB;

pub const HELP: &str = "Watch options (read-only live dashboard):
  --config PATH                 Required host TOML; the VM, device alias,
                                telemetry path and host floor come from it.
  --vm NAME                     The [[vm]] to show; required with several.
  --connect URI                 Libvirt URI; default qemu:///system.
  --interval-seconds N          Refresh period; default 1.
  --guest-interval-seconds N    Guest-agent telemetry read period; default 1.
  --events N                    Journal events shown; default 6.
  --no-guest                    Skip guest-agent reads.
  --once                        Print one plain snapshot.
Ctrl+C or q leaves a plain snapshot and the full recent events on the screen to copy.
";

/// Device samples kept for the history.
pub const HISTORY_SAMPLES: usize = 300;

#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    pub vm: String,
    pub alias: String,
    pub unit: String,
    pub telemetry_path: String,
    pub connect: String,
    pub interval: Duration,
    pub guest_interval: Duration,
    pub host_floor_bytes: u64,
    pub events: usize,
    pub guest: bool,
    pub once: bool,
}

impl Command {
    /// One VM of the configuration, with every default.
    pub fn from_config(config: &HostConfig, connect: &str) -> Self {
        Self {
            vm: config.vm_name.clone(),
            alias: config.alias.clone(),
            unit: crate::systemd_unit::CONTROLLER_UNIT.to_owned(),
            telemetry_path: config.raw_telemetry_path.clone(),
            connect: connect.to_owned(),
            interval: Duration::from_secs(1),
            guest_interval: Duration::from_secs(1),
            host_floor_bytes: config.host_min_headroom_bytes,
            events: 6,
            guest: true,
            once: false,
        }
    }
}

pub fn parse(args: &[String]) -> Result<Command, String> {
    let mut config_path = None;
    let mut vm = None;
    let mut connect = "qemu:///system".to_owned();
    let mut interval = Duration::from_secs(1);
    let mut guest_interval = Duration::from_secs(1);
    let mut events = 6;
    let mut guest = true;
    let mut once = false;
    let mut index = 0;
    while index < args.len() {
        let option = args[index].as_str();
        match option {
            "--no-guest" => guest = false,
            "--once" => once = true,
            _ => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| format!("{option} requires a value"))?;
                let seconds = || {
                    value
                        .parse::<f64>()
                        .ok()
                        .filter(|seconds| seconds.is_finite() && *seconds >= 0.1)
                        .map(Duration::from_secs_f64)
                        .ok_or_else(|| format!("{option} requires a number of seconds >= 0.1"))
                };
                match option {
                    "--config" => config_path = Some(std::path::PathBuf::from(value)),
                    "--vm" => vm = Some(value.clone()),
                    "--connect" => connect = value.clone(),
                    "--interval-seconds" => interval = seconds()?,
                    "--guest-interval-seconds" => guest_interval = seconds()?,
                    "--events" => {
                        events = value
                            .parse()
                            .ok()
                            .filter(|count| (1..=20).contains(count))
                            .ok_or("--events requires 1..=20")?;
                    }
                    _ => return Err(format!("unknown watch option: {option}\n\n{HELP}")),
                }
            }
        }
        index += 1;
    }
    let config_path = config_path.ok_or_else(|| format!("watch requires --config\n\n{HELP}"))?;
    let config = HostConfig::load_vm(&config_path, vm.as_deref())
        .map_err(|error| format!("{}: {error}", config_path.display()))?;
    let mut command = Command::from_config(&config, &connect);
    command.interval = interval;
    command.guest_interval = guest_interval;
    command.events = events;
    command.guest = guest;
    command.once = once;
    Ok(command)
}

/// The guest's latest publication, and which route it came by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GuestView {
    pub telemetry: Option<RawTelemetryEnvelope>,
    pub source: Option<PublicationSource>,
}

/// A missing agent, file or mid-write view simply leaves it empty.
fn read_guest<R: GuestFileReader>(
    files: &PublicationCopyFirst<R>,
    telemetry_path: &str,
) -> GuestView {
    let read = files
        .read_with_source(telemetry_path, MAX_TELEMETRY_BYTES)
        .ok();
    let telemetry = read
        .as_ref()
        .and_then(|(bytes, _)| serde_json::from_slice(bytes).ok());
    GuestView {
        source: telemetry.as_ref().and(read.map(|(_, source)| source)),
        telemetry,
    }
}

/// One controller event, short and in full.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub time: String,
    /// Shortened for the panel.
    pub text: String,
    /// The journal's own text from `event=`, for copying.
    pub full: String,
    pub tone: Tone,
}

/// The controller's latest `request_completed` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastResult {
    /// The journal's outcome, or "held back" when a limit held it back.
    pub outcome: String,
    pub detail: String,
    pub tone: Tone,
    /// The journal's own outcome: `Completed`, `Partial`, `PoolEmpty`, ...
    pub journal_outcome: String,
    /// What held a grant back, as `grant_limits` names it.
    pub limiting_factors: Vec<String>,
}

/// What the journal says about the VM.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Journal {
    pub events: Vec<Event>,
    pub last_result: Option<LastResult>,
    /// Why the journal gave nothing, when it couldn't be read.
    pub unreadable: Option<String>,
}

/// This VM's events, and the controller's own that name no VM.
fn read_journal(command: &Command, cwd: &Path) -> Journal {
    match journal::read_events(&command.unit, cwd) {
        Ok(text) => {
            let lines = lines_for(&text, &command.vm);
            Journal {
                events: parse_events(&lines, command.events),
                last_result: parse_last_result(&lines),
                unreadable: None,
            }
        }
        Err(reason) => Journal {
            unreadable: Some(reason),
            ..Journal::default()
        },
    }
}

/// The lines about `vm`, and the controller's own that name no VM.
pub fn lines_for<'a>(journal: &'a str, vm: &str) -> Vec<&'a str> {
    journal
        .lines()
        .filter(|line| field(line, "vm").is_none_or(|named| named == vm))
        .collect()
}

/// The newest `request_completed` among `lines`, oldest first.
pub fn parse_last_result(lines: &[&str]) -> Option<LastResult> {
    let at = lines
        .iter()
        .rposition(|line| field(line, "event") == Some("request_completed"))?;
    let line = lines[at];
    let bytes = |name: &str| field(line, name)?.parse::<u64>().ok().map(mib);
    let outcome = field(line, "outcome")?;
    let mut detail = format!(
        "{} {}M -> {}M at {}",
        field(line, "operation")?,
        bytes("bytes")?,
        bytes("actual_bytes")?,
        journal::time(line)
    );
    // A grant held back says by what: the same request's grant_limits.
    let mut shown = outcome.to_owned();
    let mut limiting_factors = Vec::new();
    if matches!(outcome, "Partial" | "PoolEmpty") {
        let sequence = field(line, "sequence");
        let limits = lines[..at].iter().rev().find(|earlier| {
            field(earlier, "event") == Some("grant_limits")
                && field(earlier, "sequence") == sequence
        });
        let factors = limits.and_then(|limits| field(limits, "limiting_factors"));
        if let Some(by) = factors.and_then(held_back_by) {
            shown = "held back".to_owned();
            detail = format!("{detail}: {by}");
        }
        limiting_factors = factors
            .map(|factors| factors.split(',').map(str::to_owned).collect())
            .unwrap_or_default();
    }
    Some(LastResult {
        detail,
        outcome: shown,
        tone: tone::outcome(outcome),
        journal_outcome: outcome.to_owned(),
        limiting_factors,
    })
}

/// What a grant's `limiting_factors` mean to a person; `None` when the
/// request itself was the limit (nothing held it back).
fn held_back_by(factors: &str) -> Option<String> {
    let words: Vec<&str> = factors
        .split(',')
        .filter_map(|factor| match factor {
            "host_headroom" => Some("the host reserve (host_reserve)"),
            "pool" => Some("the pool is fully used (pool)"),
            "guest_maximum" => Some("the VM's limit (max_ram)"),
            "device_capacity" => Some("the size of its virtio-mem device"),
            _ => None,
        })
        .collect();
    (!words.is_empty()).then(|| words.join(" and "))
}

/// The recent events with their full journal text, for copying.
pub fn full_events(events: &[Event]) -> String {
    let mut text = String::from("\nRecent controller events (full text, newest first):\n");
    for event in events {
        text.push_str(&format!("{}  {}\n", event.time, event.full));
    }
    text
}

/// Controller events worth showing, by the first matching marker.
const EVENT_TONES: [(&str, Tone); 13] = [
    ("actuation_latched", Tone::Bad),
    ("Rejected", Tone::Bad),
    ("refuse", Tone::Bad),
    ("Uncertain", Tone::Bad),
    ("error", Tone::Bad),
    // PAUSED (viomem): no longer emitted; kept for older journals.
    ("usable_region_widened", Tone::Accent),
    ("outcome=Partial", Tone::Warn),
    ("settled_short", Tone::Warn),
    ("outcome=Completed", Tone::Good),
    ("action=grant", Tone::Info),
    ("resize_requested", Tone::Info),
    ("ownership_conflict", Tone::Dim),
    ("guest_", Tone::Dim),
];

/// Newest-first events with health samples dropped, byte counts in MiB and
/// consecutive repeats (ignoring embedded timestamps) collapsed with a count.
pub fn parse_events(lines: &[&str], count: usize) -> Vec<Event> {
    let events = lines.iter().rev().filter_map(|line| {
        let (_, text) = line.split_once("event=")?;
        if text.starts_with("control_health") {
            return None;
        }
        let short = shorten(text);
        let tone = EVENT_TONES
            .iter()
            .find(|(marker, _)| short.contains(marker))
            .map_or(Tone::Normal, |(_, tone)| *tone);
        Some(Event {
            time: journal::time(line).to_owned(),
            text: short,
            full: text.to_owned(),
            tone,
        })
    });
    journal::collapse_repeats(events, count, |newer, older| {
        mask_long_numbers(&newer.text) == mask_long_numbers(&older.text)
    })
    .into_iter()
    .map(|(mut event, repeats)| {
        if repeats > 1 {
            event.text = format!("(x{repeats}) {}", event.text);
            event.full = format!("(x{repeats}) {}", event.full);
        }
        event
    })
    .collect()
}

/// Drops session IDs and shows `*bytes=N` values in MiB.
pub fn shorten(text: &str) -> String {
    text.split(' ')
        .filter(|word| !word.starts_with("session_id="))
        .map(|word| {
            word.split_once('=')
                .filter(|(key, _)| key.ends_with("bytes"))
                .and_then(|(key, value)| Some((key, value.parse::<u64>().ok()?)))
                .map_or_else(
                    || word.to_owned(),
                    |(key, value)| format!("{key}={}M", value / MIB),
                )
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn mask_long_numbers(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut digits = String::new();
    for character in text.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_digit() {
            digits.push(character);
            continue;
        }
        masked.push_str(if digits.len() >= 10 { "#" } else { &digits });
        digits.clear();
        masked.push(character);
    }
    masked.pop();
    masked
}

/// The guest's memory-pressure signal, what it means for giving memory
/// back, and its tone. A guest with no signal never returns memory, so the
/// view says why rather than showing an unknown.
pub fn memory_signal(telemetry: &RawTelemetryEnvelope) -> (&'static str, &'static str, Tone) {
    let no_psi = telemetry
        .diagnostics
        .as_ref()
        .and_then(|diagnostics| diagnostics.environment.get("pressure_signal"))
        .is_some_and(|signal| signal == "none");
    match telemetry.native_pressure {
        Some(MemoryResourceNotificationState::High) => (
            "HIGH",
            "no memory stalls: memory can be returned",
            Tone::Good,
        ),
        Some(MemoryResourceNotificationState::Neutral) => (
            "NEUTRAL",
            "some stalls: memory is kept, not returned",
            Tone::Warn,
        ),
        Some(MemoryResourceNotificationState::Low) => {
            ("LOW", "stalling on memory: asking for more", Tone::Bad)
        }
        None if no_psi => (
            "OFF",
            "no PSI in the guest: it never returns memory; boot it with psi=1",
            Tone::Bad,
        ),
        None => (
            "NONE",
            "the guest sent no signal: it never returns memory",
            Tone::Bad,
        ),
    }
}

/// Everything one refresh of a VM shows.
#[derive(Debug, Clone, PartialEq)]
pub struct VmSnapshot {
    pub command: Command,
    pub host: HostMemory,
    pub device: Option<VirtioMemState>,
    pub guest: GuestView,
    /// How long ago the guest was read; `None` before the first read.
    pub guest_age: Option<Duration>,
    /// Device samples, oldest first.
    pub history: Vec<VirtioMemState>,
    /// The guest's memory in use and its total at each device sample, as
    /// last published; `None` before a publication.
    pub guest_history: Vec<Option<(u64, u64)>>,
    pub journal: Journal,
    pub samples: u64,
}

/// Reads one VM at each refresh, keeping its device history and reading
/// the guest no more often than `guest_interval`.
pub struct VmReader {
    command: Command,
    virsh: Virsh,
    files: PublicationCopyFirst<VirshGuestFiles<Virsh>>,
    history: VecDeque<VirtioMemState>,
    guest_history: VecDeque<Option<(u64, u64)>>,
    guest: GuestView,
    guest_read: Option<Instant>,
    samples: u64,
}

impl VmReader {
    pub fn new(command: Command) -> Self {
        let virsh = Virsh::persistent_with_connection("virsh", COMMAND_TIMEOUT, &command.connect);
        Self {
            files: PublicationCopyFirst::new(
                &command.vm,
                VirshGuestFiles::new(virsh.clone(), &command.vm),
            ),
            virsh,
            command,
            history: VecDeque::new(),
            guest_history: VecDeque::new(),
            guest: GuestView::default(),
            guest_read: None,
            samples: 0,
        }
    }

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn read(&mut self, cwd: &Path) -> Result<VmSnapshot, String> {
        let started = Instant::now();
        self.samples += 1;
        let device = device_state(&self.virsh, &self.command.vm, &self.command.alias);
        if self.command.guest
            && self
                .guest_read
                .is_none_or(|read| started - read >= self.command.guest_interval)
        {
            self.guest = read_guest(&self.files, &self.command.telemetry_path);
            self.guest_read = Some(started);
        }
        if let Some(device) = device {
            let memory = self.guest.telemetry.as_ref().map(|telemetry| {
                let memory = &telemetry.memory;
                (
                    memory
                        .physical_total_bytes
                        .saturating_sub(memory.physical_available_bytes),
                    memory.physical_total_bytes,
                )
            });
            self.history.push_back(device);
            self.guest_history.push_back(memory);
            if self.history.len() > HISTORY_SAMPLES {
                self.history.pop_front();
                self.guest_history.pop_front();
            }
        }
        Ok(VmSnapshot {
            command: self.command.clone(),
            host: read_meminfo()?,
            device,
            guest: self.guest.clone(),
            guest_age: self.guest_read.map(|read| started - read),
            history: self.history.iter().copied().collect(),
            guest_history: self.guest_history.iter().copied().collect(),
            journal: read_journal(&self.command, cwd),
            samples: self.samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_vm_from_the_host_config_and_options() {
        let config = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/config");
        let config = config.to_str().expect("UTF-8 path");
        let expected = HostConfig::load_vm(Path::new(config), None).expect("tracked config");
        let defaults = parse(&["--config", config].map(str::to_owned)).expect("defaults");
        assert_eq!(defaults.vm, expected.vm_name);
        assert_eq!(defaults.alias, expected.alias);
        assert_eq!(defaults.telemetry_path, expected.raw_telemetry_path);
        assert_eq!(defaults.host_floor_bytes, expected.host_min_headroom_bytes);
        assert_eq!(defaults.unit, "rampire.service");
        assert_eq!(defaults.interval, Duration::from_secs(1));
        let command = parse(
            &[
                "--config",
                config,
                "--interval-seconds",
                "0.5",
                "--events",
                "3",
                "--no-guest",
            ]
            .map(str::to_owned),
        )
        .expect("options");
        assert_eq!(command.interval, Duration::from_millis(500));
        assert_eq!(command.events, 3);
        assert!(!command.guest);
        for bad in [
            vec![],
            vec!["--config", config, "--vm", "missing"],
            vec!["--config", config, "--interval-seconds", "0"],
            vec!["--config", config, "--events", "0"],
            vec!["--config", config, "--bogus", "1"],
            vec!["--config"],
        ] {
            assert!(parse(&bad.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
        }
    }

    #[test]
    fn events_are_newest_first_shortened_toned_and_collapsed() {
        let journal = "\
2026-10-27T09:50:31+00:00 kvm-host rampire[1]: rampire: vm=w event=guest_input_invalid reason=raw telemetry observation 1793093751508 is stale at 1793094631000
2026-10-27T09:50:37+00:00 kvm-host rampire[1]: rampire: vm=w event=guest_input_invalid reason=raw telemetry observation 1793093751508 is stale at 1793094637000
2026-10-27T09:57:19+00:00 kvm-host rampire[1]: rampire: vm=w event=controller_decision action=grant session_id=s-1 operation=Remove bytes=3221225472
2026-10-27T09:57:20+00:00 kvm-host rampire[1]: rampire: vm=w event=control_health direction=Shrink
2026-10-27T09:57:40+00:00 kvm-host rampire[1]: rampire: vm=w event=actuation_latched reason=x
";
        let lines: Vec<&str> = journal.lines().collect();
        let events = parse_events(&lines, 6);
        assert_eq!(events.len(), 3);
        assert_eq!(
            events[1].full,
            "controller_decision action=grant session_id=s-1 operation=Remove bytes=3221225472"
        );
        assert_eq!(events[0].time, "09:57:40");
        assert_eq!(events[0].tone, Tone::Bad);
        assert_eq!(
            events[1].text,
            "controller_decision action=grant operation=Remove bytes=3072M"
        );
        assert_eq!(events[1].tone, Tone::Info);
        assert!(events[2].text.starts_with("(x2) guest_input_invalid"));
        assert!(events[2].full.ends_with("is stale at 1793094637000"));
        assert_eq!(parse_events(&lines, 1).len(), 1);
        assert!(full_events(&events).contains("09:57:40  actuation_latched reason=x\n"));
    }

    #[test]
    fn only_this_vms_events_and_the_controllers_own_are_shown() {
        let journal = "2026-10-27T10:03:21+00:00 h rampire[1]: rampire: vm=w event=request_completed operation=Add bytes=1073741824 outcome=Completed actual_bytes=1073741824
2026-10-27T10:03:22+00:00 h rampire[1]: rampire: vm=other event=request_completed operation=Remove bytes=1073741824 outcome=Rejected actual_bytes=0
2026-10-27T10:03:23+00:00 h rampire[1]: rampire: event=reload_refused reason=bad file
";
        let lines = lines_for(journal, "w");
        assert_eq!(lines.len(), 2);
        assert_eq!(parse_last_result(&lines).unwrap().outcome, "Completed");
        assert!(parse_events(&lines, 6)[0]
            .text
            .starts_with("reload_refused"));
    }

    #[test]
    fn last_result_comes_from_the_newest_completion() {
        let journal = "\
2026-10-27T09:57:46+00:00 h rampire[1]: rampire: vm=w event=request_completed operation_id=1 sequence=0 operation=Remove bytes=3221225472 outcome=Partial actual_bytes=1895825408
2026-10-27T10:03:21+00:00 h rampire[1]: rampire: vm=w event=request_completed operation_id=2 sequence=1 operation=Add bytes=1073741824 outcome=Completed actual_bytes=1073741824
2026-10-27T10:03:33+00:00 h rampire[1]: rampire: vm=w event=guest_status_unpublished reason=x
";
        assert_eq!(
            parse_last_result(&journal.lines().collect::<Vec<_>>()),
            Some(LastResult {
                outcome: "Completed".to_owned(),
                detail: "Add 1024M -> 1024M at 10:03:21".to_owned(),
                tone: Tone::Good,
                journal_outcome: "Completed".to_owned(),
                limiting_factors: vec![],
            })
        );
        assert_eq!(parse_last_result(&["no completions"]), None);
    }

    #[test]
    fn a_held_back_grant_says_what_held_it_back() {
        let journal = "\
2026-11-14T16:12:59+00:00 h rampire[1]: rampire: vm=g event=grant_limits session_id=s sequence=4 operation=Add bytes=127926272 limiting_factors=host_headroom alignment_remainder_bytes=0
2026-11-14T16:12:59+00:00 h rampire[1]: rampire: vm=g event=request_completed operation_id=none session_id=s sequence=4 operation=Add bytes=127926272 outcome=PoolEmpty actual_bytes=0 reason=no free pool, device or host-reserve bytes remain
";
        assert_eq!(
            parse_last_result(&journal.lines().collect::<Vec<_>>()),
            Some(LastResult {
                outcome: "held back".to_owned(),
                detail: "Add 122M -> 0M at 16:12:59: the host reserve (host_reserve)".to_owned(),
                tone: Tone::Warn,
                journal_outcome: "PoolEmpty".to_owned(),
                limiting_factors: vec!["host_headroom".to_owned()],
            })
        );
        assert_eq!(
            held_back_by("pool,guest_maximum").as_deref(),
            Some("the pool is fully used (pool) and the VM's limit (max_ram)")
        );
        assert_eq!(held_back_by("request"), None);
    }

    #[test]
    fn a_guest_without_a_memory_signal_says_why_instead_of_unknown() {
        let text = include_str!("../../../fixtures/publication.json");
        let mut telemetry: RawTelemetryEnvelope = serde_json::from_str(text).unwrap();
        let (signal, detail, _) = memory_signal(&telemetry);
        assert_eq!(signal, "OFF");
        assert!(detail.contains("psi=1"), "{detail}");
        telemetry.diagnostics = None;
        assert_eq!(memory_signal(&telemetry).0, "NONE");
        telemetry.native_pressure = Some(MemoryResourceNotificationState::High);
        assert_eq!(memory_signal(&telemetry).0, "HIGH");
    }
}
