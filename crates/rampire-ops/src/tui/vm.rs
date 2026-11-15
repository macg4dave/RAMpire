//! One VM: its settings, its device and a resize under way, the guest's own
//! figures, the last grant and what held it back, its history and events.
//! `rampire watch` draws this alone; the TUI draws it as the VM's details.

use rampire_core::settings::VmSettings;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::theme::{Icon, Theme};
use super::widgets::{self, panel};
use crate::dashboard::{gib, mib, GIB};
use crate::reports::size;
use crate::tone::{self, Tone};
use crate::watch::{memory_signal, VmSnapshot};

/// What the details show beside the live readings: the VM's settings and
/// its boot RAM, read when it is chosen.
#[derive(Debug, Clone, PartialEq)]
pub struct VmDetails {
    pub settings: Option<VmSettings>,
    pub boot_bytes: Option<u64>,
    /// libvirt's state: running, shut off, ...
    pub state: String,
}

const SETTINGS_HEIGHT: u16 = 4;
const DEVICE_HEIGHT: u16 = 5;
const GUEST_HEIGHT: u16 = 5;

/// The rows the fixed panels take, for a plain snapshot.
pub fn fixed_height(snapshot: &VmSnapshot) -> u16 {
    SETTINGS_HEIGHT + DEVICE_HEIGHT + GUEST_HEIGHT + 2 + snapshot.command.events.clamp(1, 8) as u16
}

pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    snapshot: &VmSnapshot,
    details: Option<&VmDetails>,
) {
    let fixed = SETTINGS_HEIGHT + DEVICE_HEIGHT + GUEST_HEIGHT;
    let spare = area.height.saturating_sub(fixed);
    let events = (2 + snapshot.command.events.clamp(1, 8) as u16)
        .min(spare.saturating_sub(4).max(3))
        .min(spare);
    let (settings, device, guest, history, journal) = if area.width >= 140 {
        // Wide: the VM and its device on the left, its guest on the right.
        let top_height = SETTINGS_HEIGHT + DEVICE_HEIGHT;
        let [top, history, journal] = Layout::vertical([
            Constraint::Length(top_height),
            Constraint::Min(0),
            Constraint::Length(events),
        ])
        .areas(area);
        let [left, guest] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(1)
                .areas(top);
        let [settings, device] = Layout::vertical([
            Constraint::Length(SETTINGS_HEIGHT),
            Constraint::Min(DEVICE_HEIGHT),
        ])
        .areas(left);
        (settings, device, guest, history, journal)
    } else {
        let [settings, device, guest, history, journal] = Layout::vertical([
            Constraint::Length(SETTINGS_HEIGHT),
            Constraint::Length(DEVICE_HEIGHT),
            Constraint::Length(GUEST_HEIGHT),
            Constraint::Min(0),
            Constraint::Length(events),
        ])
        .areas(area);
        (settings, device, guest, history, journal)
    };
    settings_panel(settings, buf, theme, snapshot, details);
    device_panel(device, buf, theme, snapshot);
    guest_panel(guest, buf, theme, snapshot);
    if history.height >= 3 {
        history_panel(history, buf, theme, snapshot);
    }
    events_panel(journal, buf, theme, snapshot);
}

fn settings_panel(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    snapshot: &VmSnapshot,
    details: Option<&VmDetails>,
) {
    let command = &snapshot.command;
    let block = panel(
        theme,
        Icon::Settings,
        &command.vm,
        &format!("device {}", command.alias),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    let mut first = Vec::new();
    let mut second = Vec::new();
    match details {
        Some(details) => {
            let running = details.state == "running";
            first.push(widgets::marked(
                theme,
                if running {
                    Icon::Running
                } else {
                    Icon::Stopped
                },
                details.state.clone(),
                if running { Tone::Good } else { Tone::Bad },
            ));
            if let Some(settings) = &details.settings {
                first.extend([
                    Span::styled("  priority ", theme.text()),
                    Span::styled(settings.priority.to_string(), theme.value()),
                    Span::styled("  min_ram ", theme.text()),
                    Span::styled(super::app::byte_text(settings.min_ram), theme.value()),
                    Span::styled("  max_ram ", theme.text()),
                    Span::styled(super::app::byte_text(settings.max_ram), theme.value()),
                    Span::styled("  free_ram ", theme.text()),
                    Span::styled(super::app::byte_text(settings.free_ram), theme.value()),
                ]);
            }
            if let Some(boot) = details.boot_bytes {
                second.extend([
                    Span::styled("boot RAM ", theme.text()),
                    Span::styled(size(boot), theme.value()),
                    Span::styled("   ", theme.text()),
                ]);
            }
        }
        None => first.push(Span::styled("reading its settings...", theme.dim())),
    }
    if let Some(device) = snapshot.device {
        second.extend([
            Span::styled("device ", theme.text()),
            Span::styled(size(device.size_bytes), theme.value()),
            Span::styled(
                format!(" in {} MiB blocks   ", mib(device.block_size_bytes)),
                theme.dim(),
            ),
        ]);
    }
    let host = snapshot.host;
    let floor = command.host_floor_bytes;
    second.extend([
        Span::styled("host available ", theme.text()),
        Span::styled(
            gib(host.available),
            theme.tone(tone::floor(host.available, floor)),
        ),
        Span::styled(
            format!(" (floor {:.1} GiB)", floor as f64 / GIB as f64),
            theme.dim(),
        ),
    ]);
    Paragraph::new(vec![Line::from(first), Line::from(second)])
        .style(theme.card())
        .render(inner, buf);
}

fn device_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &VmSnapshot) {
    let block = panel(theme, Icon::Device, "DEVICE", "virtio-mem");
    let inner = block.inner(area);
    block.render(area, buf);
    let width = (inner.width as usize)
        .saturating_sub(12 + 8 + 24)
        .clamp(8, 160);
    let mut lines = Vec::new();
    match snapshot.device {
        None => lines.push(widgets::line(
            theme,
            format!(
                "{} the device can't be read: the VM is stopped or virsh failed",
                theme.icon(Icon::Problem)
            ),
            Tone::Bad,
        )),
        Some(device) => {
            let (requested, current) = (device.requested_bytes, device.current_bytes);
            lines.push(Line::from(vec![
                Span::styled("requested ", theme.text()),
                Span::styled(gib(requested), theme.value()),
                Span::styled("   plugged ", theme.text()),
                Span::styled(gib(current), theme.value()),
                Span::styled("   ", theme.text()),
                super::pool::device_state(theme, Some(device)),
                Span::styled(
                    if requested == current {
                        String::new()
                    } else {
                        format!("  {} MiB to go", mib(requested.abs_diff(current)))
                    },
                    theme.dim(),
                ),
            ]));
            lines.push(widgets::meter(
                theme,
                "Plugged",
                current as f64 / device.size_bytes.max(1) as f64,
                width,
                format!("{} of the device", size(device.size_bytes)),
                Some(Tone::Info),
            ));
        }
    }
    let journal = &snapshot.journal;
    lines.push(match &journal.last_result {
        Some(result) => Line::from(vec![
            Span::styled("last request ", theme.text()),
            widgets::marked(
                theme,
                match result.tone {
                    Tone::Good => Icon::Ok,
                    _ => Icon::Problem,
                },
                result.outcome.clone(),
                result.tone,
            ),
            Span::styled(format!("  {}", result.detail), theme.dim()),
        ]),
        None => Line::from(vec![
            Span::styled("last request ", theme.text()),
            Span::styled(
                journal
                    .unreadable
                    .clone()
                    .unwrap_or_else(|| "none in the journal".to_owned()),
                theme.dim(),
            ),
        ]),
    });
    Paragraph::new(lines).style(theme.card()).render(inner, buf);
}

fn guest_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &VmSnapshot) {
    let age = snapshot
        .guest_age
        .map_or("never".to_owned(), |age| format!("{}s ago", age.as_secs()));
    let detail = match snapshot.guest.source {
        Some(source) => format!("{}, read {age}", source.label()),
        None => format!("read {age}"),
    };
    let block = panel(theme, Icon::Guest, "GUEST", &detail);
    let inner = block.inner(area);
    block.render(area, buf);
    let width = (inner.width as usize)
        .saturating_sub(12 + 8 + 24)
        .clamp(8, 160);
    let lines = match &snapshot.guest.telemetry {
        None if !snapshot.command.guest => {
            vec![widgets::line(
                theme,
                "guest reads are off (--no-guest)",
                Tone::Dim,
            )]
        }
        None => vec![widgets::line(
            theme,
            format!(
                "{} no publication from the guest: is rampire-guest running in it?",
                theme.icon(Icon::Problem)
            ),
            Tone::Bad,
        )],
        Some(telemetry) => {
            let memory = &telemetry.memory;
            let total = memory.physical_total_bytes.max(1);
            let available = memory.physical_available_bytes;
            let (signal, meaning, signal_tone) = memory_signal(telemetry);
            vec![
                widgets::meter(
                    theme,
                    "In use",
                    1.0 - available as f64 / total as f64,
                    width,
                    format!(
                        "{:.2} of {:.2} GiB",
                        total.saturating_sub(available) as f64 / GIB as f64,
                        total as f64 / GIB as f64
                    ),
                    None,
                ),
                Line::from(vec![
                    Span::styled("available ", theme.text()),
                    Span::styled(gib(available), theme.value()),
                    Span::styled("   commit headroom ", theme.text()),
                    Span::styled(
                        gib(memory
                            .commit_limit_bytes
                            .saturating_sub(memory.commit_total_bytes)),
                        theme.value(),
                    ),
                    Span::styled("   request ", theme.text()),
                    match &telemetry.request {
                        Some(request) => Span::styled(
                            format!(
                                "{:?} {}M (sequence {})",
                                request.operation,
                                mib(request.bytes),
                                request.identity.sequence
                            ),
                            theme.tone(Tone::Info),
                        ),
                        None => Span::styled("none", theme.dim()),
                    },
                ]),
                Line::from(vec![
                    Span::styled("signal ", theme.text()),
                    Span::styled(signal, theme.tone(signal_tone)),
                    Span::styled(format!("  {meaning}"), theme.dim()),
                ]),
            ]
        }
    };
    Paragraph::new(lines).style(theme.card()).render(inner, buf);
}

fn history_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &VmSnapshot) {
    super::chart::vm_history(area, buf, theme, snapshot);
}

fn events_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &VmSnapshot) {
    let block = panel(
        theme,
        Icon::Vm,
        "EVENTS",
        &format!("{}, newest first", snapshot.command.unit),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    let journal = &snapshot.journal;
    let lines: Vec<Line> = if let Some(reason) = &journal.unreadable {
        vec![widgets::line(theme, reason.clone(), Tone::Warn)]
    } else if journal.events.is_empty() {
        vec![widgets::line(
            theme,
            "no controller events in the journal",
            Tone::Dim,
        )]
    } else {
        journal
            .events
            .iter()
            .take(inner.height as usize)
            .map(|event| {
                Line::from(vec![
                    Span::styled(format!("{}  ", event.time), theme.dim()),
                    Span::styled(event.text.clone(), theme.tone(event.tone)),
                ])
            })
            .collect()
    };
    Paragraph::new(lines).style(theme.card()).render(inner, buf);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::host_readings::HostMemory;
    use crate::tui::pool::tests::themes;
    use crate::watch::{Command, Event, GuestView, Journal};
    use rampire_core::{RawTelemetryEnvelope, VirtioMemState};

    pub(crate) fn snapshot() -> VmSnapshot {
        let device = VirtioMemState {
            size_bytes: 20 * GIB,
            block_size_bytes: 2 << 20,
            requested_bytes: GIB,
            current_bytes: 3 * GIB,
        };
        let text = include_str!("../../../../fixtures/publication.json");
        let telemetry: RawTelemetryEnvelope = serde_json::from_str(text).unwrap();
        VmSnapshot {
            command: Command {
                vm: "rhel10_rampire".to_owned(),
                alias: "ua-virtiomem0".to_owned(),
                unit: "rampire.service".to_owned(),
                telemetry_path: String::new(),
                connect: "qemu:///system".to_owned(),
                interval: std::time::Duration::from_secs(1),
                guest_interval: std::time::Duration::from_secs(1),
                host_floor_bytes: 4 * GIB,
                events: 6,
                guest: true,
                once: true,
            },
            host: HostMemory {
                total: 32 * GIB,
                available: 3 * GIB,
                swap_total: GIB,
                swap_free: GIB,
            },
            device: Some(device),
            guest: GuestView {
                telemetry: Some(telemetry),
                source: None,
            },
            guest_age: Some(std::time::Duration::ZERO),
            history: std::iter::repeat_n(device, 400).collect(),
            guest_history: std::iter::repeat_n(Some((GIB, 2 * GIB)), 400).collect(),
            journal: Journal {
                events: vec![Event {
                    time: "09:00:00".to_owned(),
                    text: "x".repeat(500),
                    full: "x".repeat(500),
                    tone: Tone::Bad,
                }],
                last_result: None,
                unreadable: None,
            },
            samples: 1,
        }
    }

    pub(crate) fn details() -> VmDetails {
        VmDetails {
            settings: rampire_core::settings::load_vm(
                &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../deploy/examples/vms.d/linux-vm.toml"),
            )
            .ok(),
            boot_bytes: Some(2 * GIB),
            state: "running".to_owned(),
        }
    }

    #[test]
    fn the_details_show_the_resize_the_guest_and_what_it_lacks() {
        for (width, height) in [(80, 21), (120, 37), (200, 57)] {
            for theme in themes() {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                render(area, &mut buf, &theme, &snapshot(), Some(&details()));
                let text = widgets::plain_text(&buf);
                assert!(text.contains("shrink -2048M"), "{text}");
                assert!(text.contains("2048 MiB to go"), "{text}");
                assert!(text.contains("signal OFF"), "{text}");
                assert!(text.contains("priority"), "{text}");
                assert!(text.contains("running"), "{text}");
            }
        }
        let theme = themes()[0];
        let mut gone = snapshot();
        gone.device = None;
        gone.guest = GuestView::default();
        let area = Rect::new(0, 0, 80, 21);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &theme, &gone, None);
        let text = widgets::plain_text(&buf);
        assert!(text.contains("the device can't be read"), "{text}");
        assert!(text.contains("no publication from the guest"), "{text}");
    }
}
