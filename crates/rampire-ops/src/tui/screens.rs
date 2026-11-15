//! Drawing the TUI: the frame (header, tabs, status line), each screen,
//! and the dialogs and overlays on top.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use super::app::{App, Dialog, Entry, Loaded, Overlay, View};
use super::jobs::Question;
use super::theme::{Icon, Theme};
use super::widgets::{self, panel};
use crate::reports::Published;
use crate::tone::Tone;

/// The smallest terminal the TUI draws in.
pub const MIN_WIDTH: u16 = 80;
pub const MIN_HEIGHT: u16 = 24;

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    render(area, frame.buffer_mut(), app);
}

/// Draw the whole TUI into `buf`.
pub fn render(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    Clear.render(area, buf);
    buf.set_style(area, theme.base());
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let text = format!(
            "RAMpire needs a terminal of at least {MIN_WIDTH}x{MIN_HEIGHT}; this one is {}x{}. Make it larger, or press q to quit.",
            area.width, area.height
        );
        Paragraph::new(text)
            .style(theme.base())
            .wrap(Wrap { trim: true })
            .render(area, buf);
        return;
    }
    let [header, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    header_bar(header, buf, app);
    match &app.view {
        View::Main => main_screen(body, buf, app),
        View::Details(vm) => details_screen(body, buf, app, vm),
        View::Doctor(vm) => doctor_screen(body, buf, app, vm.as_deref()),
        View::Logs(vm) => logs_screen(body, buf, app, vm.as_deref()),
    }
    status_bar(status, buf, app);
    if let Some(dialog) = &app.dialog {
        dialog_box(body, buf, theme, dialog);
    }
    match app.overlay {
        Some(Overlay::Help) => help(body, buf, app),
        Some(Overlay::Errors) => errors(body, buf, app),
        None => {}
    }
}

fn header_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let controller = match &app.controller {
        Some(unit) => {
            let running = unit.active_state == "active";
            Span::styled(
                format!(
                    "{} controller {} ({})",
                    theme.icon(if running {
                        Icon::Running
                    } else {
                        Icon::Problem
                    }),
                    unit.active_state,
                    unit.sub_state
                ),
                theme.bar(),
            )
        }
        None => Span::styled("controller ...", theme.bar()),
    };
    let left = Line::from(vec![
        Span::styled(format!(" {} RAMpire  ", theme.icon(Icon::Vm)), theme.bar()),
        Span::styled(format!("{}  ", short_host()), theme.bar()),
        controller,
    ]);
    // The build, without the version's words: `(build 5bac9b55a758)`.
    let build = app
        .context
        .build
        .split_once("(build ")
        .map_or(app.context.build.as_str(), |(_, rest)| {
            rest.trim_end_matches(')')
        });
    let right = format!("build {build}  {} ", crate::dashboard::clock());
    buf.set_style(area, theme.bar());
    Paragraph::new(left).style(theme.bar()).render(area, buf);
    let width = right.chars().count() as u16;
    if area.width > width + 45 {
        buf.set_string(area.right() - width, area.y, right, theme.bar());
    }
}

/// The host's name up to its first dot.
fn short_host() -> String {
    let name = crate::dashboard::host_name();
    name.split('.').next().unwrap_or(&name).to_owned()
}

/// The keys for the screen, then what last happened.
fn status_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    buf.set_style(area, theme.bar());
    let hint = match (&app.dialog, &app.view) {
        (Some(Dialog::Flow(flow)), _) if flow.done.is_some() => "Enter close  arrows scroll",
        (Some(Dialog::Flow(flow)), _) => match &flow.pending {
            Some(pending) if matches!(pending.question, Question::Text(_)) => {
                "type, Enter answer  Esc stop"
            }
            Some(_) => "y yes  n no  Esc stop",
            None => "running...  arrows scroll",
        },
        (Some(Dialog::Choice(_)), _) => "letter or Enter choose  Esc back",
        (Some(Dialog::Form(_)), _) => "type to edit  Tab next  Enter apply  Esc back",
        (None, View::Main) => "arrows choose  Enter what to do  d doctor  l logs  ? keys  q quit",
        (None, View::Details(_)) => {
            "Enter menu  s settings  o doctor  l logs  left right VM  Esc back"
        }
        (None, View::Doctor(_)) => "arrows choose  Enter run the fix  r check again  Esc back",
        (None, View::Logs(_)) => "arrows scroll  f one VM  r reload  Esc back",
    };
    let mut spans = vec![Span::styled(format!(" {hint}"), theme.bar())];
    if let Some((text, tone)) = &app.status {
        spans.push(Span::styled("  |  ", theme.bar()));
        let style = match tone {
            Tone::Bad | Tone::Warn => theme.bar().add_modifier(ratatui::style::Modifier::BOLD),
            _ => theme.bar(),
        };
        spans.push(Span::styled(text.clone(), style));
    }
    Paragraph::new(Line::from(spans))
        .style(theme.bar())
        .render(area, buf);
    if !app.errors.is_empty() {
        let text = format!(
            " {} {} error{} (e) ",
            theme.icon(Icon::Problem),
            app.errors.len(),
            if app.errors.len() == 1 { "" } else { "s" }
        );
        let width = Span::raw(text.as_str()).width() as u16;
        if area.width > width {
            buf.set_string(
                area.right() - width,
                area.y,
                text,
                theme.bar().add_modifier(ratatui::style::Modifier::BOLD),
            );
        }
    }
}

fn message(area: Rect, buf: &mut Buffer, theme: &Theme, title: &str, text: &str, tone: Tone) {
    let block = panel(theme, Icon::Vm, title, "");
    let inner = block.inner(area);
    block.render(area, buf);
    Paragraph::new(text.to_owned())
        .style(theme.tone(tone))
        .wrap(Wrap { trim: true })
        .render(inner, buf);
}

/// The one screen: the pool, every VM on the host, and the pool's history
/// and events, small.
fn main_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let pool = match &app.pool {
        Some(Ok(snapshot)) => Some(snapshot),
        _ => None,
    };
    let tall = area.height >= 36;
    let [pool_area, list_area, history_area, events_area] = Layout::vertical([
        Constraint::Length(if pool.is_some() { 6 } else { 3 }),
        Constraint::Min(5),
        Constraint::Length(if tall { 5 } else { 3 }),
        Constraint::Length(if tall { 7 } else { 4 }),
    ])
    .areas(area);
    match &app.pool {
        Some(Ok(snapshot)) => super::pool::pool_panel(pool_area, buf, theme, snapshot),
        Some(Err(error)) => message(pool_area, buf, theme, "POOL", error, Tone::Warn),
        None => message(
            pool_area,
            buf,
            theme,
            "POOL",
            "reading the pool...",
            Tone::Dim,
        ),
    }
    vm_list(list_area, buf, app, &app.entries(), pool);
    match pool {
        Some(snapshot) => {
            history_strip(history_area, buf, theme, snapshot);
            super::pool::events_panel(events_area, buf, theme, snapshot);
        }
        None => {
            message(history_area, buf, theme, "POOL HISTORY", "", Tone::Dim);
            message(events_area, buf, theme, "POOL EVENTS", "", Tone::Dim);
        }
    }
}

/// Every VM on the host, managed or not, with what Enter does for each.
fn vm_list(
    area: Rect,
    buf: &mut Buffer,
    app: &App,
    entries: &[Entry],
    pool: Option<&crate::top::PoolSnapshot>,
) {
    use crate::reports::Management;
    use ratatui::widgets::{Cell, HighlightSpacing, Row, StatefulWidget, Table, TableState};
    let theme = &app.theme;
    let block = panel(
        theme,
        Icon::Vm,
        "VMS",
        "Enter on a VM: details, settings, doctor, add, remove",
    );
    let wide = area.width >= super::pool::WIDE;
    let name_width = entries
        .iter()
        .map(|entry| entry.name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(10, 28) as u16
        + 2;
    let mut header = vec!["VM", "STATE", "PRI", "PLUGGED"];
    let mut widths = vec![
        Constraint::Length(name_width),
        Constraint::Length(13),
        Constraint::Length(4),
        Constraint::Length(8),
    ];
    if wide {
        header.push("RAM LIMITS");
        widths.push(Constraint::Length(13));
    }
    header.extend(["GUEST", "NOTE"]);
    widths.extend([Constraint::Length(17), Constraint::Min(6)]);
    let dash = || Cell::from(Span::styled("-", theme.dim()));
    let rows: Vec<Row> = entries
        .iter()
        .map(|entry| {
            let live = entry.pool_index.zip(pool);
            let mut cells = Vec::new();
            match live {
                Some((index, pool)) => {
                    let vm = &pool.command.vms[index];
                    let sample = &pool.vms[index];
                    cells.push(Cell::from(Line::from(vec![
                        Span::styled(format!("{} ", theme.icon(Icon::Full)), theme.series(index)),
                        Span::styled(entry.name.clone(), theme.value()),
                    ])));
                    cells.push(Cell::from(super::pool::device_state(theme, sample.device)));
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
                    if wide {
                        cells.push(Cell::from(Span::styled(
                            format!(
                                "{}-{}",
                                crate::top::short_gib(vm.min_ram),
                                crate::top::short_gib(vm.max_ram)
                            ),
                            theme.dim(),
                        )));
                    }
                    let (guest, request) = super::pool::guest_cells(
                        theme,
                        sample.guest.as_ref(),
                        pool.now_unix_millis,
                    );
                    cells.push(Cell::from(guest));
                    // A pending request, or a stale publication; nothing when quiet.
                    let quiet = request.to_string() == "none";
                    cells.push(Cell::from(if quiet { Line::default() } else { request }));
                }
                None => {
                    let (icon, tone, next) = match entry.management {
                        Management::Managed => (Icon::Vm, Tone::Value, "details"),
                        Management::Parked => (Icon::Parked, Tone::Dim, "bring back"),
                        Management::Unfinished => (Icon::Problem, Tone::Warn, "finish"),
                        Management::NotManaged => (Icon::Stopped, Tone::Dim, "add"),
                    };
                    cells.push(Cell::from(widgets::marked(
                        theme,
                        icon,
                        entry.name.clone(),
                        tone,
                    )));
                    cells.push(Cell::from(Span::styled(
                        entry.management.label().to_owned(),
                        theme.tone(tone),
                    )));
                    cells.push(dash());
                    cells.push(dash());
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
            }
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
        .highlight_spacing(HighlightSpacing::Always);
    let selected = (!empty).then(|| app.cursor.min(entries.len() - 1));
    let mut state = TableState::new().with_selected(selected);
    StatefulWidget::render(table, area, buf, &mut state);
    let note = match (&app.inventory, empty) {
        (Loaded::NotYet | Loaded::Loading, _) => Some(("reading the host's VMs...", Tone::Dim)),
        (Loaded::Done(Err(_)), _) => {
            Some(("the host's VMs couldn't be read: r tries again", Tone::Warn))
        }
        (_, true) => Some(("libvirt has no VMs on this connection", Tone::Dim)),
        _ => None,
    };
    if let Some((text, tone)) = note {
        let row = area.y + 2 + entries.len() as u16;
        if row + 1 < area.bottom() {
            let line = Rect {
                y: row,
                height: 1,
                ..area.inner(ratatui::layout::Margin::new(2, 0))
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
        Published::No => {
            widgets::marked(theme, Icon::Problem, "not installed".to_owned(), Tone::Warn)
        }
        Published::Unknown => Span::styled("unknown", theme.dim()),
    }
}

/// The pool's charge over time, as one strip of bars.
fn history_strip(area: Rect, buf: &mut Buffer, theme: &Theme, snapshot: &crate::top::PoolSnapshot) {
    use ratatui::widgets::Sparkline;
    let scale = snapshot.scale().max(1);
    let width = area.width.saturating_sub(2) as usize;
    let totals: Vec<u64> = snapshot
        .history
        .iter()
        .rev()
        .take(width)
        .rev()
        .map(|charges| charges.iter().copied().fold(0, u64::saturating_add))
        .collect();
    let block = panel(
        theme,
        Icon::Pool,
        "POOL HISTORY",
        &format!(
            "charged, last {} samples, 0..{:.1} GiB",
            totals.len(),
            scale as f64 / crate::dashboard::GIB as f64
        ),
    );
    Sparkline::default()
        .block(block)
        .data(&totals)
        .max(scale)
        .bar_set(theme.icons.bars())
        .style(theme.tone(Tone::Info))
        .render(area, buf);
}

/// One managed VM live: the `watch` view.
fn details_screen(area: Rect, buf: &mut Buffer, app: &App, vm: &str) {
    match &app.vm {
        Some((shown, Ok(snapshot))) if shown == vm => {
            super::vm::render(area, buf, &app.theme, snapshot, app.details.get(vm))
        }
        Some((shown, Err(error))) if shown == vm => {
            message(area, buf, &app.theme, vm, error, Tone::Warn)
        }
        _ => message(area, buf, &app.theme, vm, "reading the VM...", Tone::Dim),
    }
}

fn loading<T>(
    area: Rect,
    buf: &mut Buffer,
    theme: &Theme,
    title: &str,
    loaded: &Loaded<T>,
) -> bool {
    match loaded {
        Loaded::NotYet | Loaded::Loading => {
            message(area, buf, theme, title, "reading...", Tone::Dim);
            true
        }
        Loaded::Done(Err(error)) => {
            message(area, buf, theme, title, error, Tone::Bad);
            true
        }
        Loaded::Done(Ok(_)) => false,
    }
}

fn doctor_screen(area: Rect, buf: &mut Buffer, app: &App, vm: Option<&str>) {
    let theme = &app.theme;
    let title = match vm {
        Some(vm) => format!("DOCTOR {vm}"),
        None => "DOCTOR".to_owned(),
    };
    if loading(area, buf, theme, &title, &app.doctor) {
        return;
    }
    let rows = app.doctor_rows();
    let problems = rows.iter().filter(|(_, finding)| !finding.ok).count();
    let block = panel(
        theme,
        Icon::Ok,
        &title,
        &format!(
            "{}, problems first: {problems} problem{}",
            if vm.is_some() {
                "this VM"
            } else {
                "the host and each VM"
            },
            if problems == 1 { "" } else { "s" }
        ),
    );
    let inner = block.inner(area);
    block.render(area, buf);
    let cursor = app.doctor_cursor.min(rows.len().saturating_sub(1));
    let mut lines: Vec<(Line, bool)> = Vec::new();
    let mut section: Option<Option<String>> = None;
    for (index, (vm, finding)) in rows.iter().enumerate() {
        if section.as_ref() != Some(vm) {
            section = Some(vm.clone());
            let title = match vm {
                None => format!("{} the host", theme.icon(Icon::Host)),
                Some(vm) => format!("{} {vm}", theme.icon(Icon::Vm)),
            };
            lines.push((Line::from(Span::styled(title, theme.title())), false));
        }
        let selected = index == cursor;
        let (icon, tone) = if finding.ok {
            (Icon::Ok, Tone::Good)
        } else {
            (Icon::Problem, Tone::Bad)
        };
        lines.push((
            Line::from(vec![
                widgets::marked(theme, icon, format!("{}:", finding.check), tone),
                Span::styled(format!(" {}", finding.detail), theme.text()),
            ]),
            selected,
        ));
        if let Some(fix) = &finding.fix {
            let runnable = super::actions::from_fix(fix).is_some();
            lines.push((
                Line::from(Span::styled(
                    format!(
                        "      fix{}: {fix}",
                        if runnable && selected {
                            " [Enter runs it]"
                        } else {
                            ""
                        }
                    ),
                    theme.tone(Tone::Warn),
                )),
                false,
            ));
        }
    }
    // Each line wrapped to the screen, the chosen finding's rows marked.
    let mut rows: Vec<(Line, bool)> = Vec::new();
    for (line, selected) in lines {
        let prefix = if selected {
            Span::styled(format!("{} ", theme.icon(Icon::Pointer)), theme.selected())
        } else {
            Span::styled("  ", theme.base())
        };
        let spans: Vec<Span> = std::iter::once(prefix)
            .chain(line.spans.into_iter().map(|span| {
                if selected {
                    span.style(theme.selected())
                } else {
                    span
                }
            }))
            .collect();
        for row in hard_wrap(Line::from(spans), inner.width as usize) {
            rows.push((row, selected));
        }
    }
    // Keep the chosen finding, and the fix beneath it, in view.
    let last = rows
        .iter()
        .rposition(|(_, selected)| *selected)
        .unwrap_or(0);
    let height = inner.height as usize;
    let start = (last + 3).saturating_sub(height);
    let shown: Vec<Line> = rows
        .into_iter()
        .skip(start)
        .take(height)
        .map(|(line, _)| line)
        .collect();
    Paragraph::new(shown).style(theme.base()).render(inner, buf);
}

fn logs_screen(area: Rect, buf: &mut Buffer, app: &App, vm: Option<&str>) {
    let theme = &app.theme;
    let filter = match vm {
        Some(vm) => format!("lines about {vm} (f: next VM)"),
        None => "every line (f: one VM)".to_owned(),
    };
    if let Loaded::Done(Ok(text)) = &app.log {
        let block = panel(
            theme,
            Icon::Vm,
            "LOGS",
            &format!("the controller's journal, {filter}"),
        );
        let inner = block.inner(area);
        block.render(area, buf);
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() {
            Paragraph::new(widgets::line(theme, "no journal lines", Tone::Dim)).render(inner, buf);
            return;
        }
        let height = inner.height as usize;
        let end = lines
            .len()
            .saturating_sub(app.log_scroll.min(lines.len().saturating_sub(1)));
        let start = end.saturating_sub(height);
        let shown: Vec<Line> = lines[start..end]
            .iter()
            .map(|line| {
                let tone = if line.contains("event=operator_change") {
                    Tone::Accent
                } else if line.contains("latched")
                    || line.contains("error")
                    || line.contains("refuse")
                {
                    Tone::Bad
                } else if line.contains("Partial") || line.contains("warning") {
                    Tone::Warn
                } else {
                    Tone::Normal
                };
                widgets::line(
                    theme,
                    widgets::fit(&compact_log_line(line), inner.width as usize),
                    tone,
                )
            })
            .collect();
        Paragraph::new(shown).style(theme.base()).render(inner, buf);
        return;
    }
    loading(area, buf, theme, "LOGS", &app.log);
}

/// A journal line as the logs screen shows it: the time, then the message
/// without the host, the identifier and the `rampire:` prefix.
pub fn compact_log_line(line: &str) -> String {
    let mut words = line.splitn(4, ' ');
    let (Some(stamp), Some(_host), Some(ident), Some(message)) =
        (words.next(), words.next(), words.next(), words.next())
    else {
        return line.to_owned();
    };
    if !ident.ends_with(':') || stamp.len() < 19 {
        return line.to_owned();
    }
    let message = message.strip_prefix("rampire: ").unwrap_or(message);
    let ident = ident
        .split('[')
        .next()
        .unwrap_or(ident)
        .trim_end_matches(':');
    let time = stamp.get(11..19).unwrap_or(stamp);
    if ident == crate::audit::IDENTIFIER {
        format!("{time} {message}")
    } else {
        format!("{time} {ident}: {message}")
    }
}

/// A box in the middle of `area`, at most `width` by `height`.
fn centred(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

fn dialog_box(area: Rect, buf: &mut Buffer, theme: &Theme, dialog: &Dialog) {
    let width = area.width.saturating_sub(4).min(120);
    let height = area.height.saturating_sub(2);
    match dialog {
        Dialog::Flow(flow) => {
            let rect = centred(area, width, height);
            Clear.render(rect, buf);
            let detail = match &flow.done {
                None => "running".to_owned(),
                Some(Ok(_)) => "done".to_owned(),
                Some(Err(_)) => "failed".to_owned(),
            };
            let block = panel(theme, Icon::Settings, &flow.title, &detail);
            let inner = block.inner(rect);
            block.render(rect, buf);
            let mut lines: Vec<Line> = flow
                .lines
                .iter()
                .map(|(text, tone)| widgets::line(theme, text.clone(), *tone))
                .collect();
            match (&flow.pending, &flow.done) {
                (Some(pending), _) => {
                    let prompt = match &pending.question {
                        Question::Confirm(text) | Question::Review(text) => format!("{text} [y/N]"),
                        Question::Text(text) => format!("{text} {}", pending.input),
                    };
                    lines.push(Line::from(widgets::marked(
                        theme,
                        Icon::Pointer,
                        prompt,
                        Tone::Accent,
                    )));
                }
                (None, Some(Ok(result))) => lines.push(Line::from(widgets::marked(
                    theme,
                    Icon::Ok,
                    result.clone(),
                    Tone::Good,
                ))),
                (None, Some(Err(error))) => {
                    lines.push(Line::from(widgets::marked(
                        theme,
                        Icon::Problem,
                        error.clone(),
                        Tone::Bad,
                    )));
                    lines.push(widgets::line(
                        theme,
                        "kept in the errors list (e); the journal has the change log: journalctl -t rampire",
                        Tone::Dim,
                    ));
                }
                (None, None) => {}
            }
            // Newest at the bottom, scrolled back by `scroll`. Lines are
            // wrapped here, so the rows counted are the rows drawn.
            let rows: Vec<Line> = lines
                .into_iter()
                .flat_map(|line| hard_wrap(line, inner.width as usize))
                .collect();
            let height = inner.height as usize;
            let end = rows.len().saturating_sub(flow.scroll);
            let start = end.saturating_sub(height);
            Paragraph::new(rows[start..end].to_vec())
                .style(theme.base())
                .render(inner, buf);
        }
        Dialog::Choice(choice) => {
            // One line per item; what the chosen one does, and the command
            // that does the same, beneath them.
            let box_width = width.min(90);
            let inner_width = box_width.saturating_sub(2) as usize;
            let mut lines: Vec<Line<'static>> = choice
                .text
                .iter()
                .flat_map(|text| {
                    hard_wrap(
                        widgets::line(theme, text.clone(), Tone::Normal),
                        inner_width,
                    )
                })
                .collect();
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            for (index, option) in choice.choices.iter().enumerate() {
                let style = if index == choice.cursor {
                    theme.selected()
                } else {
                    theme.value()
                };
                lines.push(Line::from(Span::styled(
                    format!(" {}  {} ", option.key, option.label),
                    style,
                )));
            }
            lines.push(Line::default());
            if let Some(option) = choice.choices.get(choice.cursor) {
                lines.extend(hard_wrap(
                    widgets::line(theme, option.detail.clone(), Tone::Dim),
                    inner_width,
                ));
            }
            lines.push(widgets::line(
                theme,
                "letter or Enter: do it   Esc: back, nothing changed",
                Tone::Dim,
            ));
            let rect = centred(area, box_width, (lines.len() as u16 + 2).min(height));
            Clear.render(rect, buf);
            let block = panel(theme, Icon::Settings, &choice.title, "");
            let inner = block.inner(rect);
            block.render(rect, buf);
            Paragraph::new(lines).style(theme.base()).render(inner, buf);
        }
        Dialog::Form(form) => {
            let rect = centred(
                area,
                width.min(70),
                (form.fields.len() as u16 + 6).min(height),
            );
            Clear.render(rect, buf);
            let block = panel(
                theme,
                Icon::Settings,
                &format!("{} settings", form.vm),
                "applied when saved",
            );
            let inner = block.inner(rect);
            block.render(rect, buf);
            let mut lines = Vec::new();
            for (index, field) in form.fields.iter().enumerate() {
                let style = if index == form.cursor {
                    theme.selected()
                } else {
                    theme.value()
                };
                let changed = if field.value.trim() != field.original {
                    format!("  (was {})", field.original)
                } else {
                    String::new()
                };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {:<10}", field.key), theme.text()),
                    Span::styled(format!(" {:<16}", field.value), style),
                    Span::styled(changed, theme.dim()),
                ]));
            }
            lines.push(Line::default());
            let changes = form.changes();
            let command = if changes.is_empty() {
                "no changes yet".to_owned()
            } else {
                let mut words = vec!["set", form.vm.as_str()];
                words.extend(changes.iter().map(String::as_str));
                format!("Enter runs: rampire {}", words.join(" "))
            };
            lines.push(widgets::line(theme, command, Tone::Info));
            lines.push(widgets::line(theme, "sizes like 4GiB or 512MiB", Tone::Dim));
            Paragraph::new(lines).style(theme.base()).render(inner, buf);
        }
    }
}

/// `line` cut into rows of at most `width` columns, keeping each span's
/// style.
fn hard_wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    // Words with the spaces after them, each with its span's style.
    let mut words: Vec<(String, ratatui::style::Style)> = Vec::new();
    for span in line.spans {
        let mut word = String::new();
        for character in span.content.chars() {
            if character != ' ' && word.ends_with(' ') {
                words.push((std::mem::take(&mut word), span.style));
            }
            word.push(character);
        }
        if !word.is_empty() {
            words.push((word, span.style));
        }
    }
    let cells = |text: &str| Span::raw(text.to_owned()).width();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (word, style) in words {
        let trimmed = cells(word.trim_end());
        if used > 0 && used + trimmed > width {
            rows.push(Line::from(std::mem::take(&mut row)));
            used = 0;
        }
        if trimmed <= width {
            used += cells(&word);
            row.push(Span::styled(word, style));
            continue;
        }
        // A word wider than a row is cut where the row ends.
        let mut piece = String::new();
        for character in word.chars() {
            let cell = cells(&character.to_string());
            if used + cell > width {
                row.push(Span::styled(std::mem::take(&mut piece), style));
                rows.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            piece.push(character);
            used += cell;
        }
        row.push(Span::styled(piece, style));
    }
    rows.push(Line::from(row));
    rows
}

fn help(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let rect = centred(area, 76, 22);
    Clear.render(rect, buf);
    let block = panel(theme, Icon::Settings, "KEYS", "Esc closes");
    let inner = block.inner(rect);
    block.render(rect, buf);
    let rows: [(&str, &str); 18] = [
        ("arrows, j k", "choose a VM; Home, End, PgUp, PgDn"),
        ("Enter", "what can be done with the chosen VM"),
        ("v", "its details, live"),
        ("s", "its settings: priority, min_ram, max_ram, free_ram"),
        ("o", "its doctor"),
        ("a", "add it, or attest it again"),
        (
            "k, f, b",
            "remove it keeping its settings, forget it, bring it back",
        ),
        ("d", "the doctor, for the host and every VM"),
        ("l", "the logs"),
        ("Esc", "back, or close a dialog"),
        ("r", "read again"),
        ("e", "the errors list"),
        ("t", "next colour theme: navy, light, terminal"),
        ("i", "Unicode or ASCII icons"),
        ("q, Ctrl+C", "quit"),
        ("", ""),
        ("", "A VM's menu shows only what fits it, with its key."),
        (
            "",
            "Every action shows the rampire command that does the same.",
        ),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(format!(" {key:<16}"), theme.value()),
                Span::styled((*what).to_owned(), theme.text()),
            ])
        })
        .collect();
    Paragraph::new(lines).style(theme.base()).render(inner, buf);
}

fn errors(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let rect = centred(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    Clear.render(rect, buf);
    let block = panel(
        theme,
        Icon::Problem,
        "ERRORS",
        "this session's, newest last; Esc closes",
    );
    let inner = block.inner(rect);
    block.render(rect, buf);
    let mut lines = Vec::new();
    for entry in &app.errors {
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", entry.time), theme.dim()),
            Span::styled(entry.what.clone(), theme.value()),
        ]));
        lines.push(widgets::line(
            theme,
            format!("   {}", entry.error),
            Tone::Bad,
        ));
    }
    Paragraph::new(lines)
        .style(theme.base())
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::actions::Action;
    use crate::tui::app::{Choice, ChoiceDialog, ErrorEntry, FlowDialog, Pick};
    use crate::tui::jobs::{DoctorSection, Msg};
    use crate::tui::pool::tests::themes;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

    fn app() -> crate::tui::app::App {
        let (mut app, _rx) = crate::tui::app::tests::app();
        app.on_msg(Msg::Vm(
            "vm0".to_owned(),
            Box::new(Ok(crate::tui::vm::tests::snapshot())),
        ));
        app.on_msg(Msg::Details(
            "vm0".to_owned(),
            crate::tui::vm::tests::details(),
        ));
        app.on_msg(Msg::Doctor(Ok(vec![
            DoctorSection {
                vm: Some("vm0".to_owned()),
                findings: vec![
                    crate::doctor::Finding::problem(
                        "attestation",
                        "drifted: domain_xml_sha256",
                        "if the change was intended, rampire vm attest vm0",
                    ),
                    crate::doctor::Finding::ok("latch", "none"),
                ],
            },
            DoctorSection {
                vm: None,
                findings: vec![crate::doctor::Finding::ok("libvirt", "11.0.0")],
            },
        ])));
        app.on_msg(Msg::Log(Ok(
            "2026-11-14T10:00:00+0000 pc101 rampire[1]: rampire: vm=vm0 event=operator_change action=set user=dave via=tui\n"
                .repeat(50),
        )));
        app
    }

    fn text(app: &crate::tui::app::App, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, app);
        widgets::plain_text(&buf)
    }

    #[test]
    fn every_view_draws_at_every_size_in_every_theme() {
        let mut app = app();
        let expected = [
            (View::Main, "POOL EVENTS"),
            (View::Details("vm0".to_owned()), "VIRTIO-MEM DEVICE"),
            (View::Doctor(None), "drifted"),
            (View::Logs(None), "operator_change"),
        ];
        for theme in themes() {
            app.theme = theme;
            for (view, needle) in &expected {
                app.view = view.clone();
                for (width, height) in SIZES {
                    let text = text(&app, width, height);
                    assert!(
                        text.contains(needle),
                        "{view:?} at {width}x{height} lacks {needle}:\n{text}"
                    );
                    assert!(text.contains("RAMpire"), "{text}");
                    assert_eq!(text.lines().count(), height as usize, "{view:?}\n{text}");
                }
            }
        }
    }

    #[test]
    fn the_main_screen_lists_every_vm_with_what_enter_does() {
        let app = app();
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            for needle in [
                "vm0",
                "grow +1024M",
                "oldvm",
                "parked",
                "bring",
                "newvm",
                "not managed",
                "not installed",
                "POOL HISTORY",
            ] {
                assert!(
                    text.contains(needle),
                    "{width}x{height} lacks {needle}:\n{text}"
                );
            }
        }
    }

    #[test]
    fn a_vms_menu_draws_with_its_keys_and_commands() {
        let mut app = app();
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            for needle in [
                "vm0 (managed)",
                "v  Details, live",
                "s  Settings",
                "o  Doctor",
                "Forget it",
            ] {
                assert!(
                    text.contains(needle),
                    "{width}x{height} lacks {needle}:\n{text}"
                );
            }
        }
        app.on_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let text = text(&app, 120, 40);
        assert!(
            text.contains("vm add will add a 6144 MiB virtio-mem device"),
            "{text}"
        );
        assert!(text.contains("apt install rampire-guest"), "{text}");
        assert!(text.contains("a  Add it"), "{text}");
    }

    #[test]
    fn log_lines_keep_the_time_and_the_message() {
        assert_eq!(
            compact_log_line("2026-11-14T10:00:00+0000 pc101 rampire[12]: rampire: vm=web event=x"),
            "10:00:00 vm=web event=x"
        );
        assert_eq!(
            compact_log_line("2026-11-14T10:00:00+0000 pc101 systemd[1]: Started rampire.service."),
            "10:00:00 systemd: Started rampire.service."
        );
        assert_eq!(compact_log_line("-- No entries --"), "-- No entries --");
    }

    #[test]
    fn a_long_dialog_shows_its_newest_line_whatever_wraps() {
        let mut app = app();
        let (reply, _answer) = std::sync::mpsc::channel();
        let long = "word ".repeat(30);
        app.dialog = Some(Dialog::Flow(FlowDialog {
            action: Action::Apply,
            title: "rampire apply".to_owned(),
            lines: std::iter::repeat_n((long, Tone::Normal), 40).collect(),
            pending: Some(crate::tui::app::Pending {
                question: Question::Confirm("Apply this and restart rampire.service?".to_owned()),
                reply,
                input: String::new(),
            }),
            done: None,
            scroll: 0,
        }));
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            assert!(text.contains("restart rampire.service? [y/N]"), "{text}");
        }
        let rows = hard_wrap(Line::from("abcdef"), 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].to_string(), "ef");
        let rows = hard_wrap(Line::from("one two three"), 8);
        assert_eq!(
            rows.iter().map(|row| row.to_string()).collect::<Vec<_>>(),
            ["one two ", "three"]
        );
    }

    #[test]
    fn a_small_terminal_says_the_size_it_needs() {
        let app = app();
        let text = text(&app, 79, 24);
        assert!(text.contains("at least 80x24"), "{text}");
        assert!(text.contains("79x24"), "{text}");
    }

    #[test]
    fn dialogs_show_the_command_line_the_question_and_the_error() {
        let mut app = app();
        let (reply, _answer) = std::sync::mpsc::channel();
        app.dialog = Some(Dialog::Flow(FlowDialog {
            action: Action::Add("newvm".to_owned()),
            title: "rampire vm add newvm --fix-xml".to_owned(),
            lines: vec![("--- saved definition".to_owned(), Tone::Dim)],
            pending: Some(crate::tui::app::Pending {
                question: Question::Confirm(
                    "Change newvm's saved definition like this?".to_owned(),
                ),
                reply,
                input: String::new(),
            }),
            done: None,
            scroll: 0,
        }));
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            assert!(text.contains("rampire vm add newvm --fix-xml"), "{text}");
            assert!(text.contains("like this? [y/N]"), "{text}");
            assert!(text.contains("y yes"), "{text}");
        }
        app.dialog = Some(Dialog::Choice(ChoiceDialog {
            title: "Run the fix".to_owned(),
            text: vec!["This runs: rampire apply".to_owned()],
            choices: vec![Choice {
                key: 'y',
                label: "Run it".to_owned(),
                detail: "its own questions come next".to_owned(),
                pick: Pick::Run(Action::Apply),
            }],
            cursor: 0,
        }));
        assert!(text(&app, 80, 24).contains("Run it"));
        app.dialog = None;
        app.errors.push(ErrorEntry {
            time: "10:00:00".to_owned(),
            what: "rampire apply".to_owned(),
            error: "the draft did not pass its checks".to_owned(),
        });
        assert!(text(&app, 80, 24).contains("1 error (e)"));
        app.overlay = Some(Overlay::Errors);
        assert!(text(&app, 80, 24).contains("the draft did not pass its checks"));
        app.overlay = Some(Overlay::Help);
        assert!(text(&app, 80, 24).contains("what can be done with the chosen VM"));
    }

    #[test]
    fn the_doctor_offers_to_run_a_fix_that_is_a_command() {
        let mut app = app();
        app.view = View::Doctor(None);
        let text = text(&app, 120, 40);
        assert!(text.contains("[Enter runs it]"), "{text}");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Dialog::Choice(choice)) = &app.dialog else {
            panic!("a dialog to confirm the fix");
        };
        assert_eq!(
            choice.choices[0].pick,
            Pick::Run(Action::Attest {
                vm: "vm0".to_owned(),
                restart: false
            })
        );
    }
}
