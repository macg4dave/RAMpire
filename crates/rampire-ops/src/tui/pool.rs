//! The dashboard: the pool's charge split by VM, every managed VM, the
//! pool's history and its events. `rampire top` draws this alone; the TUI
//! draws it as its first screen with a VM selected.

use rampire_core::RawTelemetryEnvelope;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, StatefulWidget, Table, TableState, Widget};

use super::theme::{Icon, Theme};
use super::widgets::{self, panel, StackedHistory};
use crate::dashboard::{mib, GIB};
use crate::tone::{self, Tone};
use crate::top::{short_gib, PoolSnapshot, STALE_PUBLICATION};

/// Below this width the VM table drops its RAM limits.
pub const WIDE: u16 = 110;

/// The rows the dashboard's fixed panels take, so a plain snapshot is as
/// tall as it needs: the pool, the VM table and the events.
pub fn fixed_height(snapshot: &PoolSnapshot) -> u16 {
    POOL_HEIGHT + table_height(snapshot) + events_height(snapshot)
}

const POOL_HEIGHT: u16 = 6;

fn table_height(snapshot: &PoolSnapshot) -> u16 {
    3 + snapshot.command.vms.len().max(1) as u16
}

fn events_height(snapshot: &PoolSnapshot) -> u16 {
    2 + snapshot.command.events.clamp(1, 8) as u16
}

/// Draw the dashboard in `area`, with the VM at `selected` highlighted.
pub fn render(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    snapshot: &PoolSnapshot,
    selected: Option<usize>,
) {
    let events = events_height(snapshot);
    let table = table_height(snapshot);
    // At 24 rows the history shrinks first, then the events.
    let spare = area.height.saturating_sub(POOL_HEIGHT + table);
    let events = events.min(spare.saturating_sub(4).max(3)).min(spare);
    let [pool, vms, history, journal] = Layout::vertical([
        Constraint::Length(POOL_HEIGHT),
        Constraint::Length(table),
        Constraint::Min(0),
        Constraint::Length(events),
    ])
    .areas(area);
    pool_panel(pool, buf, theme, snapshot);
    vm_table(vms, buf, theme, snapshot, selected);
    if history.height >= 3 {
        history_panel(history, buf, theme, snapshot);
    }
    events_panel(journal, buf, theme, snapshot);
}

fn pool_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &PoolSnapshot) {
    let block = panel(
        theme,
        Icon::Pool,
        "POOL",
        "charge = max(requested, plugged) per VM",
    );
    let inner = block.inner(area);
    block.render(area, buf);
    let charges = snapshot.charges();
    let charged = snapshot.charged();
    let scale = snapshot.scale();
    let fraction = charged as f64 / scale.max(1) as f64;
    let cells = (inner.width as usize).saturating_sub(12 + 8);

    let mut bar = vec![Span::styled(format!("{:<12}", "Charged"), theme.text())];
    bar.extend(widgets::stacked_bar(theme, &charges, scale, cells));
    bar.push(Span::styled(
        format!(" {:5.1}%", fraction * 100.0),
        theme.tone(tone::load(fraction)),
    ));

    let mut legend = vec![Span::styled(" ".repeat(12), theme.text())];
    for (index, vm) in snapshot.command.vms.iter().enumerate() {
        legend.push(Span::styled(theme.icon(Icon::Full), theme.series(index)));
        legend.push(Span::styled(format!(" {}  ", vm.name), theme.text()));
    }

    let session = &snapshot.session;
    let sizes = match (snapshot.command.pool_bytes, snapshot.free()) {
        (Some(pool), Some(free)) => {
            let free_tone = if free == 0 {
                Tone::Bad
            } else if free < pool / 10 {
                Tone::Warn
            } else {
                Tone::Good
            };
            Line::from(vec![
                Span::styled("Pool ", theme.text()),
                Span::styled(one_place(pool), theme.value()),
                Span::styled("   charged ", theme.text()),
                Span::styled(one_place(charged), theme.tone(tone::load(fraction))),
                Span::styled(
                    format!(" peak {}", one_place(session.peak_charged)),
                    theme.dim(),
                ),
                Span::styled("   free ", theme.text()),
                Span::styled(one_place(free), theme.tone(free_tone)),
                Span::styled(
                    format!(" low {}", one_place(session.low_free.unwrap_or(free))),
                    theme.dim(),
                ),
            ])
        }
        _ => Line::from(vec![
            Span::styled("Charged ", theme.text()),
            Span::styled(one_place(charged), theme.value()),
            Span::styled("   single-VM controller: no pool", theme.dim()),
        ]),
    };

    let (moving, growing, shrinking) = snapshot.resizing();
    let host = snapshot.host;
    let floor = snapshot.command.host_floor_bytes;
    let mut state = vec![Span::styled("Resizing ", theme.text())];
    if moving == 0 {
        state.push(widgets::marked(
            theme,
            Icon::Steady,
            "none".to_owned(),
            Tone::Good,
        ));
    } else {
        let mut moves = Vec::new();
        if growing > 0 {
            moves.push(format!("+{}M", mib(growing)));
        }
        if shrinking > 0 {
            moves.push(format!("-{}M", mib(shrinking)));
        }
        state.push(widgets::marked(
            theme,
            if growing >= shrinking {
                Icon::Growing
            } else {
                Icon::Shrinking
            },
            format!(
                "{moving} VM{} {}",
                if moving == 1 { "" } else { "s" },
                moves.join(" ")
            ),
            Tone::Info,
        ));
    }
    let floor_tone = tone::floor(host.available, floor);
    state.extend([
        Span::styled("   host available ", theme.text()),
        Span::styled(one_place(host.available), theme.tone(floor_tone)),
        Span::styled(
            format!(
                " floor {} ({:+.1}) low {}",
                one_place(floor),
                (host.available as f64 - floor as f64) / GIB as f64,
                one_place(session.low_host_available.unwrap_or(host.available))
            ),
            theme.dim(),
        ),
    ]);
    Paragraph::new(vec![
        Line::from(bar),
        Line::from(legend),
        sizes,
        Line::from(state),
    ])
    .style(theme.base())
    .render(inner, buf);
}

/// GiB to one place, for the dashboard's summary lines.
fn one_place(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// A device's state in words with its symbol: steady, growing, shrinking.
pub fn device_state<'a>(theme: &Theme, device: Option<rampire_core::VirtioMemState>) -> Span<'a> {
    match device {
        None => widgets::marked(theme, Icon::Stopped, "off".to_owned(), Tone::Bad),
        Some(device) => {
            let (requested, current) = (device.requested_bytes, device.current_bytes);
            match requested.cmp(&current) {
                std::cmp::Ordering::Equal => {
                    widgets::marked(theme, Icon::Steady, "steady".to_owned(), Tone::Good)
                }
                std::cmp::Ordering::Greater => widgets::marked(
                    theme,
                    Icon::Growing,
                    format!("grow +{}M", mib(requested - current)),
                    Tone::Info,
                ),
                std::cmp::Ordering::Less => widgets::marked(
                    theme,
                    Icon::Shrinking,
                    format!("shrink -{}M", mib(current - requested)),
                    Tone::Warn,
                ),
            }
        }
    }
}

fn vm_table(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    snapshot: &PoolSnapshot,
    selected: Option<usize>,
) {
    let block = panel(theme, Icon::Vm, "VMS", "higher priority is served first");
    let wide = area.width >= WIDE;
    let name_width = snapshot
        .command
        .vms
        .iter()
        .map(|vm| vm.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(10, 31) as u16
        + 2;
    let mut header = vec!["VM", "PRI", "PLUGGED", "DEVICE"];
    let mut widths = vec![
        Constraint::Length(name_width),
        Constraint::Length(4),
        Constraint::Length(8),
        Constraint::Length(14),
    ];
    if wide {
        header.push("RAM LIMITS");
        widths.push(Constraint::Length(13));
    }
    header.extend(["GUEST IN USE", "REQUEST"]);
    widths.extend([Constraint::Length(18), Constraint::Min(6)]);

    let rows: Vec<Row> = snapshot
        .command
        .vms
        .iter()
        .zip(&snapshot.vms)
        .enumerate()
        .map(|(index, (vm, sample))| {
            let mut cells = vec![
                Cell::from(Line::from(vec![
                    Span::styled(format!("{} ", theme.icon(Icon::Full)), theme.series(index)),
                    Span::styled(vm.name.clone(), theme.value()),
                ])),
                Cell::from(Span::styled(format!("{:>3}", vm.priority), theme.text())),
                Cell::from(Span::styled(
                    sample
                        .device
                        .map_or_else(|| "-".to_owned(), |device| short_gib(device.current_bytes)),
                    theme.value(),
                )),
                Cell::from(device_state(theme, sample.device)),
            ];
            if wide {
                cells.push(Cell::from(Span::styled(
                    format!("{}-{}", short_gib(vm.min_ram), short_gib(vm.max_ram)),
                    theme.dim(),
                )));
            }
            let (guest, request) =
                guest_cells(theme, sample.guest.as_ref(), snapshot.now_unix_millis);
            cells.push(Cell::from(guest));
            cells.push(Cell::from(request));
            Row::new(cells)
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(theme.dim()))
        .block(block)
        .style(theme.base())
        .row_highlight_style(theme.selected())
        .highlight_symbol(Line::from(theme.icon(Icon::Pointer)))
        .highlight_spacing(ratatui::widgets::HighlightSpacing::Always);
    let mut state = TableState::new().with_selected(selected);
    StatefulWidget::render(table, area, buf, &mut state);
    if empty {
        let inner = Rect {
            y: area.y + 2,
            height: 1,
            ..area.inner(ratatui::layout::Margin::new(2, 0))
        };
        Paragraph::new(widgets::line(
            theme,
            "RAMpire manages no VMs yet: press 3 to add one",
            Tone::Dim,
        ))
        .render(inner, buf);
    }
}

/// The guest's in-use meter and its pending request.
fn guest_cells<'a>(
    theme: &Theme,
    guest: Option<&RawTelemetryEnvelope>,
    now_unix_millis: u64,
) -> (Line<'a>, Line<'a>) {
    let Some(guest) = guest else {
        return (
            widgets::line(theme, "no publication", Tone::Dim),
            Line::default(),
        );
    };
    let memory = &guest.memory;
    let total = memory.physical_total_bytes.max(1);
    let fraction = 1.0 - memory.physical_available_bytes as f64 / total as f64;
    let mut meter = widgets::bar(theme, fraction, 4, tone::load(fraction));
    meter.push(Span::styled(
        format!(
            " {:3.0}% of {}",
            fraction * 100.0,
            short_gib(memory.physical_total_bytes)
        ),
        theme.tone(tone::load(fraction)),
    ));
    let mut request = match &guest.request {
        Some(request) => vec![Span::styled(
            format!("{:?} {}M", request.operation, mib(request.bytes)),
            theme.tone(Tone::Info),
        )],
        None => vec![Span::styled("none", theme.dim())],
    };
    let age = std::time::Duration::from_millis(
        now_unix_millis.saturating_sub(guest.observed_unix_millis),
    );
    if age >= STALE_PUBLICATION {
        request.push(Span::styled(
            format!(" {} stale {}s", theme.icon(Icon::Problem), age.as_secs()),
            theme.tone(Tone::Warn),
        ));
    }
    (Line::from(meter), Line::from(request))
}

fn history_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &PoolSnapshot) {
    let scale = snapshot.scale().max(1);
    let shown = snapshot
        .history
        .len()
        .min(area.width.saturating_sub(8) as usize);
    let block = panel(
        theme,
        Icon::Pool,
        "POOL HISTORY",
        &format!(
            "last {shown} samples, stacked by VM, 0..{:.1} GiB",
            scale as f64 / GIB as f64
        ),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    StackedHistory {
        theme,
        history: &snapshot.history,
        scale,
    }
    .render(inner, buf);
}

fn events_panel(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &PoolSnapshot) {
    let block = panel(theme, Icon::Pool, "POOL EVENTS", "every VM, newest first");
    let inner = block.inner(area);
    block.render(area, buf);
    let lines: Vec<Line> = match &snapshot.events {
        Err(reason) => vec![widgets::line(theme, reason.clone(), Tone::Warn)],
        Ok(events) if events.is_empty() => {
            vec![widgets::line(
                theme,
                "no pool events in the journal",
                Tone::Dim,
            )]
        }
        Ok(events) => {
            let width = events
                .iter()
                .map(|event| event.vm.chars().count())
                .max()
                .unwrap_or(0)
                .clamp(10, 31);
            events
                .iter()
                .take(inner.height as usize)
                .map(|event| {
                    let index = snapshot
                        .command
                        .vms
                        .iter()
                        .position(|vm| vm.name == event.vm);
                    Line::from(vec![
                        Span::styled(format!("{}  ", event.time), theme.dim()),
                        Span::styled(
                            format!("{:<width$} ", event.vm),
                            index.map_or(theme.text(), |index| theme.series(index)),
                        ),
                        Span::styled(event.text.clone(), theme.tone(event.tone)),
                    ])
                })
                .collect()
        }
    };
    Paragraph::new(lines).style(theme.base()).render(inner, buf);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::host_readings::HostMemory;
    use crate::top::{Command, PoolEvent, Session, Vm, VmSample};
    use crate::tui::theme::{Depth, Icons, Palette};
    use rampire_core::VirtioMemState;

    pub(crate) fn snapshot(vms: usize) -> PoolSnapshot {
        let vm = |index: usize| Vm {
            name: if index == 1 {
                "a-very-long-vm-name".to_owned()
            } else {
                format!("vm{index}")
            },
            alias: "ua-virtiomem0".to_owned(),
            priority: 100,
            min_ram: GIB,
            max_ram: 8 * GIB,
        };
        let device = |requested: u64, current: u64| {
            Some(VirtioMemState {
                size_bytes: 6 * GIB,
                block_size_bytes: 2 << 20,
                requested_bytes: requested,
                current_bytes: current,
            })
        };
        let text = include_str!("../../../../fixtures/publication.json");
        let guest: RawTelemetryEnvelope = serde_json::from_str(text).unwrap();
        PoolSnapshot {
            command: Command {
                vms: (0..vms).map(vm).collect(),
                pool_bytes: Some(12 * GIB),
                host_floor_bytes: 4 * GIB,
                unit: "rampire.service".to_owned(),
                connect: "qemu:///system".to_owned(),
                interval: std::time::Duration::from_secs(1),
                events: 8,
                once: true,
            },
            host: HostMemory {
                total: 64 * GIB,
                available: 20 * GIB,
                swap_total: 0,
                swap_free: 0,
            },
            vms: (0..vms)
                .map(|index| VmSample {
                    device: match index % 3 {
                        0 => device(2 * GIB, GIB),
                        1 => None,
                        _ => device(GIB, GIB),
                    },
                    guest: (index % 3 != 1).then(|| guest.clone()),
                })
                .collect(),
            now_unix_millis: guest.observed_unix_millis + 1000,
            samples: 3,
            session: Session::default(),
            history: std::iter::repeat_n(vec![GIB; vms], 400).collect(),
            events: Ok(vec![PoolEvent {
                time: "10:00:00".to_owned(),
                vm: "vm0".to_owned(),
                text: "x".repeat(300),
                tone: Tone::Bad,
            }]),
        }
    }

    pub(crate) fn themes() -> Vec<Theme> {
        let mut themes = Vec::new();
        for palette in Palette::ALL {
            for depth in [Depth::Colours256, Depth::Colours16, Depth::None] {
                for unicode in [true, false] {
                    themes.push(Theme::new(palette, depth, Icons { unicode }));
                }
            }
        }
        themes
    }

    #[test]
    fn the_dashboard_says_what_each_vm_is_doing_in_words() {
        for (width, height) in [(80, 21), (120, 37), (200, 57)] {
            for theme in themes() {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                render(area, &mut buf, &theme, &snapshot(3), Some(0));
                let text = widgets::plain_text(&buf);
                assert!(text.contains("grow +1024M"), "{width}x{height}\n{text}");
                assert!(text.contains("off"), "{text}");
                assert!(text.contains("steady"), "{text}");
                assert!(text.contains("a-very-long-vm-name"), "{text}");
                assert!(text.contains("POOL EVENTS"), "{text}");
                assert_eq!(text.contains("RAM LIMITS"), width >= WIDE, "{text}");
            }
        }
    }

    #[test]
    fn a_pool_with_no_vms_says_how_to_add_one() {
        let theme = themes()[0];
        let area = Rect::new(0, 0, 80, 21);
        let mut buf = Buffer::empty(area);
        let mut empty = snapshot(0);
        empty.events = Err("journal unreadable".to_owned());
        render(area, &mut buf, &theme, &empty, None);
        let text = widgets::plain_text(&buf);
        assert!(text.contains("press 3 to add one"), "{text}");
        assert!(text.contains("journal unreadable"), "{text}");
    }
}
