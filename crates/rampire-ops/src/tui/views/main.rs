//! The one screen: the pool, every VM on the host, the pool's history and
//! events; on a wide, tall window the chosen VM's details beside the list,
//! as Yazi previews the file under its cursor.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Cell, HighlightSpacing, Paragraph, Row, StatefulWidget, Table, TableState, Widget,
};

use super::message;
use crate::reports::{Management, Published};
use crate::tone::Tone;
use crate::top::PoolSnapshot;
use crate::tui::app::{App, Entry, Loaded};
use crate::tui::list::Linemode;
use crate::tui::theme::{Icon, Theme};
use crate::tui::widgets::{self, panel};

/// Where each part goes.
struct Areas {
    pool: Rect,
    list: Rect,
    history: Rect,
    events: Rect,
    preview: Option<Rect>,
}

/// Wide windows put the pool beside its events, the history across the
/// whole width under them, and the list beside the preview when there is
/// height for it. Tall windows give the history room for its chart.
fn areas(area: Rect, app: &App) -> Areas {
    let pool = matches!(app.pool, Some(Ok(_)));
    let tall = area.height >= 36;
    if area.width >= 140 {
        let top_height = if tall { 9 } else { 7 };
        let [top, history, bottom] = Layout::vertical([
            Constraint::Length(top_height),
            Constraint::Length(if tall { 12 } else { 6 }),
            Constraint::Min(5),
        ])
        .areas(area);
        let [left, events] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .spacing(1)
                .areas(top);
        let (list, preview) = if bottom.height >= 20 {
            let [list, preview] =
                Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)])
                    .spacing(1)
                    .areas(bottom);
            (list, Some(preview))
        } else {
            (bottom, None)
        };
        return Areas {
            pool: left,
            list,
            history,
            events,
            preview,
        };
    }
    let [pool_area, list, history, events] = Layout::vertical([
        Constraint::Length(match (pool, tall) {
            (false, _) => 3,
            (true, false) => 6,
            (true, true) => crate::tui::pool::POOL_HEIGHT_ROOMY,
        }),
        Constraint::Min(5),
        Constraint::Length(if tall { 9 } else { 4 }),
        Constraint::Length(if tall { 6 } else { 4 }),
    ])
    .areas(area);
    Areas {
        pool: pool_area,
        list,
        history,
        events,
        preview: None,
    }
}

/// The rows of VMs the list has room for: inside its border, under its
/// header.
fn list_rows(list: Rect) -> usize {
    list.height.saturating_sub(3) as usize
}

/// The screen row of the chosen VM, for its menu to open beside.
pub fn row_y(area: Rect, app: &App) -> Option<u16> {
    let list = areas(area, app).list;
    let total = app.visible().len();
    if total == 0 {
        return None;
    }
    let cursor = app.list.scroll.cursor(total);
    let offset = app.list.scroll.offset();
    let row = cursor.checked_sub(offset)?;
    (row < list_rows(list)).then(|| list.y + 2 + row as u16)
}

pub fn render(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let pool = match &app.pool {
        Some(Ok(snapshot)) => Some(snapshot),
        _ => None,
    };
    let areas = areas(area, app);
    match &app.pool {
        Some(Ok(snapshot)) => crate::tui::pool::pool_panel(areas.pool, buf, theme, snapshot),
        Some(Err(error)) => message(areas.pool, buf, theme, "POOL", error, Tone::Warn),
        None => message(areas.pool, buf, theme, "POOL", "reading...", Tone::Dim),
    }
    let entries = app.visible();
    vm_list(areas.list, buf, app, &entries, pool);
    match pool {
        Some(snapshot) => {
            crate::tui::chart::pool_history(areas.history, buf, theme, snapshot);
            crate::tui::pool::events_panel(areas.events, buf, theme, snapshot);
        }
        None => {
            message(areas.history, buf, theme, "HISTORY", "", Tone::Dim);
            message(areas.events, buf, theme, "EVENTS", "", Tone::Dim);
        }
    }
    if let Some(preview_area) = areas.preview {
        preview(preview_area, buf, app);
    }
}

/// The chosen VM beside the list: its details live, or its facts.
fn preview(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let Some(entry) = app.selected() else {
        return message(area, buf, theme, "PREVIEW", "no VM chosen", Tone::Dim);
    };
    if entry.management != Management::Managed {
        return crate::tui::spot::render_preview(area, buf, app, &entry);
    }
    match &app.vm {
        Some((shown, Ok(snapshot))) if *shown == entry.name => {
            crate::tui::vm::render(area, buf, theme, snapshot, app.details.get(&entry.name))
        }
        Some((shown, Err(error))) if *shown == entry.name => {
            message(area, buf, theme, &entry.name, error, Tone::Warn)
        }
        _ => message(
            area,
            buf,
            theme,
            &entry.name,
            "reading the VM...",
            Tone::Dim,
        ),
    }
}

/// Every VM on the host, managed or not, with what Enter does for each.
fn vm_list(
    area: Rect,
    buf: &mut Buffer,
    app: &App,
    entries: &[Entry],
    pool: Option<&PoolSnapshot>,
) {
    let theme = &app.theme;
    let detail = match (&app.list.filter, app.list.sort) {
        (Some(filter), sort) => format!("/{filter}, {}", sort.label()),
        (None, crate::tui::list::SortBy::Pool) => "Enter: menu  Tab: facts".to_owned(),
        (None, sort) => format!("by {}", sort.label()),
    };
    let block = panel(theme, Icon::Vm, "VMS", &detail);
    let wide = area.width >= crate::tui::pool::WIDE;
    let name_width = entries
        .iter()
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(10, 28) as u16
        + 2
        // A mark's room, while any VM is marked.
        + if app.list.marks.is_empty() { 0 } else { 2 };
    let mut header = vec!["VM", "STATE", "PRI", "PLUGGED"];
    let mut widths = vec![
        Constraint::Length(name_width),
        Constraint::Length(14),
        Constraint::Length(4),
        Constraint::Length(8),
    ];
    let mode = app.list.linemode;
    let guest_mode = mode == Linemode::Guest;
    if guest_mode && wide {
        header.push("LIMITS");
        widths.push(Constraint::Length(12));
    }
    // The guest's gauge takes the width the fixed columns leave, so a
    // wide window shows it long rather than gaps between columns.
    let fixed = name_width + 14 + 4 + 8 + if guest_mode && wide { 12 } else { 0 };
    let spare = area.width.saturating_sub(
        fixed + 4 /* borders, padding */ + 2 /* marker */ + 6 /* gaps */ + 14, /* note */
    );
    let gauge = spare.saturating_sub(13).clamp(4, 60) as usize;
    match mode {
        Linemode::Guest => {
            header.extend(["GUEST", "NOTE"]);
            widths.extend([Constraint::Length(gauge as u16 + 13), Constraint::Fill(1)]);
        }
        Linemode::Limits => {
            header.extend(["MIN", "MAX", "FREE"]);
            widths.extend([
                Constraint::Length(9),
                Constraint::Length(9),
                Constraint::Fill(1),
            ]);
        }
        Linemode::Device => {
            header.extend(["REQUESTED", "SIZE", "BLOCK"]);
            widths.extend([
                Constraint::Length(10),
                Constraint::Length(9),
                Constraint::Fill(1),
            ]);
        }
        Linemode::Detail => {
            header.push("VM LIST SAYS");
            widths.push(Constraint::Fill(1));
        }
    }
    let rows_shown = list_rows(area);
    let window = app.list.scroll.window(entries.len(), rows_shown);
    let cursor = app.list.scroll.cursor(entries.len());
    let selected = window.contains(&cursor).then(|| cursor - window.start);
    let dash = || Cell::from(Span::styled("-", theme.dim()));
    let rows: Vec<Row> = entries[window]
        .iter()
        .map(|entry| {
            let marked = app.list.marks.contains(&entry.name);
            let mark = if marked {
                Span::styled(format!("{} ", theme.icon(Icon::Ok)), theme.accent())
            } else {
                Span::raw("")
            };
            let live = entry.pool_index.zip(pool);
            let mut cells = Vec::new();
            match live {
                Some((index, pool)) => {
                    let vm = &pool.command.vms[index];
                    let sample = &pool.vms[index];
                    cells.push(Cell::from(Line::from(vec![
                        mark,
                        Span::styled(format!("{} ", theme.icon(Icon::Full)), theme.series(index)),
                        Span::styled(entry.name.clone(), theme.value()),
                    ])));
                    cells.push(Cell::from(crate::tui::pool::device_state(
                        theme,
                        sample.device,
                    )));
                    cells.push(Cell::from(Span::styled(
                        format!("{:>3}", vm.priority),
                        theme.text(),
                    )));
                    cells.push(Cell::from(Span::styled(
                        sample.device.map_or_else(
                            || "-".to_owned(),
                            |device| crate::top::short_gib(device.current_bytes),
                        ),
                        theme.value(),
                    )));
                    let short = crate::top::short_gib;
                    match mode {
                        Linemode::Guest => {
                            if wide {
                                cells.push(Cell::from(Span::styled(
                                    format!("{}-{}", short(vm.min_ram), short(vm.max_ram)),
                                    theme.dim(),
                                )));
                            }
                            let (guest, request) = crate::tui::pool::guest_cells_sized(
                                theme,
                                sample.guest.as_ref(),
                                pool.now_unix_millis,
                                gauge,
                            );
                            cells.push(Cell::from(guest));
                            // A pending request, or a stale publication; else an
                            // update for its guest service; nothing when quiet.
                            let quiet = request.to_string() == "none";
                            cells.push(Cell::from(if !quiet {
                                request
                            } else if app.update_available(entry) {
                                Line::from(Span::styled(
                                    "update available (u)",
                                    theme.tone(Tone::Warn),
                                ))
                            } else {
                                Line::default()
                            }));
                        }
                        Linemode::Limits => {
                            let free = app
                                .details
                                .get(&entry.name)
                                .and_then(|details| details.settings.as_ref())
                                .map_or("-".to_owned(), |settings| short(settings.free_ram));
                            for text in [short(vm.min_ram), short(vm.max_ram), free] {
                                cells.push(Cell::from(Span::styled(text, theme.text())));
                            }
                        }
                        Linemode::Device => match sample.device {
                            Some(device) => {
                                for text in [
                                    short(device.requested_bytes),
                                    short(device.size_bytes),
                                    format!(
                                        "{} MiB",
                                        crate::dashboard::mib(device.block_size_bytes)
                                    ),
                                ] {
                                    cells.push(Cell::from(Span::styled(text, theme.text())));
                                }
                            }
                            None => cells.extend([dash(), dash(), dash()]),
                        },
                        Linemode::Detail => cells.push(Cell::from(Span::styled(
                            entry
                                .listed
                                .as_ref()
                                .map_or(String::new(), |listed| listed.detail.clone()),
                            theme.dim(),
                        ))),
                    }
                }
                None => {
                    let (icon, tone, next) = match entry.management {
                        Management::Managed => (Icon::Vm, Tone::Value, "details"),
                        Management::Unfinished => (Icon::Problem, Tone::Warn, "finish"),
                        Management::NotManaged => (Icon::Stopped, Tone::Dim, "add"),
                    };
                    cells.push(Cell::from(Line::from(vec![
                        mark,
                        widgets::marked(theme, icon, entry.name.clone(), tone),
                    ])));
                    cells.push(Cell::from(Span::styled(
                        entry.management.label().to_owned(),
                        theme.tone(tone),
                    )));
                    cells.push(dash());
                    cells.push(dash());
                    match mode {
                        Linemode::Guest => {
                            if wide {
                                cells.push(dash());
                            }
                            cells.push(Cell::from(match &entry.listed {
                                Some(listed) => guest_short(theme, &listed.guest),
                                None => Span::styled("", theme.dim()),
                            }));
                            let blocked = entry
                                .listed
                                .as_ref()
                                .is_some_and(|listed| !listed.problems.is_empty());
                            cells.push(Cell::from(if blocked {
                                Span::styled("fix first", theme.tone(Tone::Warn))
                            } else {
                                Span::styled(next, theme.tone(Tone::Info))
                            }));
                        }
                        Linemode::Limits | Linemode::Device => {
                            cells.extend([dash(), dash(), dash()])
                        }
                        Linemode::Detail => cells.push(Cell::from(Span::styled(
                            entry
                                .listed
                                .as_ref()
                                .map_or(String::new(), |listed| listed.detail.clone()),
                            theme.dim(),
                        ))),
                    }
                }
            }
            Row::new(cells)
        })
        .collect();
    let empty = rows.is_empty();
    let table = Table::new(rows, widths)
        .header(Row::new(header).style(theme.dim()))
        .block(block)
        .style(theme.card())
        .row_highlight_style(theme.selected_row())
        .highlight_symbol(Line::styled(theme.icon(Icon::Pointer), theme.accent()))
        .highlight_spacing(HighlightSpacing::Always);
    let mut state = TableState::new().with_selected(selected);
    StatefulWidget::render(table, area, buf, &mut state);
    let note = match (&app.inventory, empty, &app.list.filter) {
        (Loaded::NotYet | Loaded::Loading, _, _) => Some(("reading the host's VMs...", Tone::Dim)),
        (Loaded::Done(Err(_)), _, _) => {
            Some(("the host's VMs couldn't be read: r tries again", Tone::Warn))
        }
        (_, true, Some(_)) => Some(("no VM's name has the filter: Esc clears it", Tone::Dim)),
        (_, true, None) => Some(("libvirt has no VMs on this connection", Tone::Dim)),
        _ => None,
    };
    if let Some((text, tone)) = note {
        let row = area.y + 2 + entries.len().min(rows_shown) as u16;
        if row + 1 < area.bottom() {
            let line = Rect {
                y: row,
                height: 1,
                ..area.inner(Margin::new(2, 0))
            };
            Paragraph::new(widgets::line(theme, text, tone)).render(line, buf);
        }
    }
}

/// A VM's guest service in a word, for the list.
fn guest_short<'a>(theme: &Theme, guest: &Published) -> Span<'a> {
    match guest {
        Published::Yes { .. } => {
            widgets::marked(theme, Icon::Ok, "publishing".to_owned(), Tone::Good)
        }
        Published::Installed => {
            widgets::marked(theme, Icon::Ok, "installed".to_owned(), Tone::Good)
        }
        Published::No => Span::styled("no guest service", theme.dim()),
        Published::Unknown => Span::styled("unknown", theme.dim()),
    }
}
