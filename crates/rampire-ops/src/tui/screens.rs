//! Drawing the TUI: the frame (header, tabs, status line), each screen,
//! and the dialogs and overlays on top.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use super::app::{App, Dialog, Loaded, Overlay, Screen};
use super::jobs::Question;
use super::theme::{Icon, Theme};
use super::widgets::{self, panel};
use crate::reports::{Management, Published};
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
    let [header, tabs, body, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    header_bar(header, buf, app);
    tab_bar(tabs, buf, app);
    match app.screen {
        Screen::Dashboard => dashboard(body, buf, app),
        Screen::Vm => vm_screen(body, buf, app),
        Screen::Add => add_screen(body, buf, app),
        Screen::Remove => remove_screen(body, buf, app),
        Screen::Doctor => doctor_screen(body, buf, app),
        Screen::Logs => logs_screen(body, buf, app),
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
        Span::styled(format!("{}  ", crate::dashboard::host_name()), theme.bar()),
        controller,
    ]);
    let right = format!("{}  {} ", app.context.build, crate::dashboard::clock());
    buf.set_style(area, theme.bar());
    Paragraph::new(left).style(theme.bar()).render(area, buf);
    let width = right.chars().count() as u16;
    if area.width > width + 50 {
        buf.set_string(area.right() - width, area.y, right, theme.bar());
    }
}

fn tab_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let mut spans = vec![Span::styled(" ", theme.base())];
    for (index, screen) in Screen::ALL.iter().enumerate() {
        let style = if *screen == app.screen {
            theme.selected()
        } else {
            theme.dim()
        };
        let mut title = screen.title().to_owned();
        if *screen == Screen::Vm {
            if let Some(vm) = app.selected_vm() {
                title = format!("VM {}", widgets::fit(&vm, 18));
            }
        }
        spans.push(Span::styled(format!(" {} {title} ", index + 1), style));
        spans.push(Span::styled(" ", theme.base()));
    }
    Paragraph::new(Line::from(spans))
        .style(theme.base())
        .render(area, buf);
}

/// The keys for the screen, then what last happened.
fn status_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    buf.set_style(area, theme.bar());
    let hint = match (&app.dialog, app.screen) {
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
        (None, Screen::Dashboard) => "arrows choose  Enter details  Tab screens  ? keys  q quit",
        (None, Screen::Vm) => "s settings  a attest  x remove  d doctor  l logs  Esc back",
        (None, Screen::Add) => "arrows choose  Enter add  r reload  ? keys",
        (None, Screen::Remove) => "arrows choose  Enter remove or bring back  r reload",
        (None, Screen::Doctor) => "arrows choose  Enter run the fix  r check again",
        (None, Screen::Logs) => "arrows scroll  f filter by VM  r reload",
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

fn dashboard(area: Rect, buf: &mut Buffer, app: &App) {
    match &app.pool {
        None => message(
            area,
            buf,
            &app.theme,
            "DASHBOARD",
            "reading the pool...",
            Tone::Dim,
        ),
        Some(Err(error)) => message(area, buf, &app.theme, "DASHBOARD", error, Tone::Warn),
        Some(Ok(snapshot)) => super::pool::render(
            area,
            buf,
            &app.theme,
            snapshot,
            (!snapshot.command.vms.is_empty()).then_some(app.selected),
        ),
    }
}

fn vm_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let Some(name) = app.selected_vm() else {
        return message(
            area,
            buf,
            &app.theme,
            "VM",
            "RAMpire manages no VMs yet: press 3 to add one.",
            Tone::Dim,
        );
    };
    match &app.vm {
        Some((shown, Ok(snapshot))) if *shown == name => {
            super::vm::render(area, buf, &app.theme, snapshot, app.details.get(&name))
        }
        Some((shown, Err(error))) if *shown == name => {
            message(area, buf, &app.theme, &name, error, Tone::Warn)
        }
        _ => message(area, buf, &app.theme, &name, "reading the VM...", Tone::Dim),
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

fn published_span<'a>(theme: &Theme, guest: &Published) -> Span<'a> {
    match guest {
        Published::Yes { age_seconds } => widgets::marked(
            theme,
            Icon::Ok,
            format!("guest published {age_seconds}s ago"),
            Tone::Good,
        ),
        Published::No => widgets::marked(
            theme,
            Icon::Problem,
            "guest not published".to_owned(),
            Tone::Warn,
        ),
        Published::Unknown => Span::styled("guest unknown", theme.dim()),
    }
}

/// A list on the left and the chosen item's details on the right; one
/// above the other on a narrow screen.
fn list_and_details(area: Rect) -> (Rect, Rect) {
    if area.width >= 120 {
        let [list, details] =
            Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)])
                .areas(area);
        (list, details)
    } else {
        let [list, details] =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(area);
        (list, details)
    }
}

fn list_lines<'a>(theme: &Theme, rows: Vec<(Line<'a>, bool)>, height: usize) -> Vec<Line<'a>> {
    let selected = rows.iter().position(|(_, selected)| *selected).unwrap_or(0);
    let start = selected.saturating_sub(height.saturating_sub(1));
    rows.into_iter()
        .skip(start)
        .take(height)
        .map(|(line, selected)| {
            if selected {
                let mut spans = vec![Span::styled(
                    format!("{} ", theme.icon(Icon::Pointer)),
                    theme.selected(),
                )];
                spans.extend(
                    line.spans
                        .into_iter()
                        .map(|span| span.style(theme.selected())),
                );
                Line::from(spans)
            } else {
                let mut spans = vec![Span::styled("  ", theme.base())];
                spans.extend(line.spans);
                Line::from(spans)
            }
        })
        .collect()
}

fn add_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    if loading(area, buf, theme, "ADD A VM", &app.inventory) {
        return;
    }
    let vms = app.addable();
    let (list, details) = list_and_details(area);
    let block = panel(
        theme,
        Icon::Vm,
        "ADD A VM",
        "the host's VMs RAMpire doesn't manage",
    );
    let inner = block.inner(list);
    block.render(list, buf);
    if vms.is_empty() {
        Paragraph::new(widgets::line(
            theme,
            "every VM on this host is managed or parked (see 4 Remove to bring one back)",
            Tone::Dim,
        ))
        .wrap(Wrap { trim: true })
        .render(inner, buf);
        return;
    }
    let cursor = app.add_cursor.min(vms.len() - 1);
    let rows = vms
        .iter()
        .enumerate()
        .map(|(index, vm)| {
            let mut spans = vec![
                Span::styled(
                    format!("{:<20} ", widgets::fit(&vm.vm_name, 20)),
                    theme.value(),
                ),
                Span::styled(format!("{:<9} ", vm.state), theme.dim()),
            ];
            if vm.management == Management::Unfinished {
                spans.push(Span::styled("unfinished  ", theme.tone(Tone::Warn)));
            }
            spans.push(published_span(theme, &vm.guest));
            (Line::from(spans), index == cursor)
        })
        .collect();
    Paragraph::new(list_lines(theme, rows, inner.height as usize))
        .style(theme.base())
        .render(inner, buf);

    let vm = vms[cursor];
    let block = panel(
        theme,
        Icon::Settings,
        &vm.vm_name,
        "what vm add --fix-xml does",
    );
    let inner = block.inner(details);
    block.render(details, buf);
    let mut lines = vec![Line::from(Span::styled(vm.detail.clone(), theme.dim()))];
    if vm.management == Management::Unfinished {
        lines.push(widgets::line(
            theme,
            "An earlier vm add didn't finish: Enter finishes it.",
            Tone::Warn,
        ));
    }
    if vm.to_fix.is_empty() && vm.management == Management::NotManaged {
        lines.push(widgets::line(
            theme,
            "Its definition needs no change.",
            Tone::Good,
        ));
    }
    for change in &vm.to_fix {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} ", theme.icon(Icon::Settings)),
                theme.tone(Tone::Info),
            ),
            Span::styled(change.clone(), theme.text()),
        ]));
    }
    for problem in &vm.problems {
        lines.push(Line::from(widgets::marked(
            theme,
            Icon::Problem,
            problem.problem.clone(),
            Tone::Bad,
        )));
        lines.push(Line::from(Span::styled(
            format!("   fix: {}", problem.fix),
            theme.tone(Tone::Warn),
        )));
    }
    if !matches!(vm.guest, Published::Yes { .. }) {
        lines.push(Line::default());
        lines.push(Line::from(widgets::marked(
            theme,
            Icon::Guest,
            "Its guest service hasn't published. Install it in the VM first:".to_owned(),
            Tone::Warn,
        )));
        lines.push(Line::from(Span::styled(
            format!("   {}", super::app::install_command(vm.os_id.as_deref())),
            theme.value(),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("Enter runs: ", theme.dim()),
        Span::styled(
            app.context
                .command_line(&["vm", "add", &vm.vm_name, "--fix-xml"]),
            theme.value(),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        "It shows the definition change, asks the review question, writes the VM's files, restarts it if needed and applies.",
        theme.dim(),
    )));
    Paragraph::new(lines)
        .style(theme.base())
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn remove_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    if loading(area, buf, theme, "REMOVE A VM", &app.inventory) {
        return;
    }
    let vms = app.removable();
    let (list, details) = list_and_details(area);
    let block = panel(
        theme,
        Icon::Vm,
        "REMOVE A VM",
        "managed VMs, and parked ones to bring back",
    );
    let inner = block.inner(list);
    block.render(list, buf);
    if vms.is_empty() {
        Paragraph::new(widgets::line(
            theme,
            "RAMpire manages no VMs and has none parked",
            Tone::Dim,
        ))
        .render(inner, buf);
        return;
    }
    let cursor = app.remove_cursor.min(vms.len() - 1);
    let rows = vms
        .iter()
        .enumerate()
        .map(|(index, vm)| {
            let (icon, tone) = match vm.management {
                Management::Parked => (Icon::Parked, Tone::Dim),
                _ => (Icon::Vm, Tone::Value),
            };
            (
                Line::from(vec![
                    widgets::marked(
                        theme,
                        icon,
                        format!("{:<20}", widgets::fit(&vm.vm_name, 20)),
                        tone,
                    ),
                    Span::styled(format!(" {:<11} ", vm.management.label()), theme.dim()),
                    Span::styled(vm.state.clone(), theme.dim()),
                ]),
                index == cursor,
            )
        })
        .collect();
    Paragraph::new(list_lines(theme, rows, inner.height as usize))
        .style(theme.base())
        .render(inner, buf);
    let vm = vms[cursor];
    let choice = super::app::remove_choice(
        &vm.vm_name,
        vm.management == Management::Parked,
        &app.context,
    );
    let block = panel(theme, Icon::Settings, &vm.vm_name, vm.management.label());
    let inner = block.inner(details);
    block.render(details, buf);
    let mut lines = vec![Line::from(Span::styled(vm.detail.clone(), theme.dim()))];
    lines.extend(
        choice
            .text
            .iter()
            .map(|text| widgets::line(theme, text.clone(), Tone::Normal)),
    );
    lines.push(Line::default());
    for option in &choice.choices {
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", option.key), theme.value()),
            Span::styled(option.label.clone(), theme.value()),
        ]));
        lines.push(Line::from(Span::styled(
            format!("   {}", option.detail),
            theme.dim(),
        )));
        lines.push(Line::from(Span::styled(
            format!("   {}", option.action.command_line(&app.context)),
            theme.tone(Tone::Info),
        )));
    }
    Paragraph::new(lines)
        .style(theme.base())
        .wrap(Wrap { trim: false })
        .render(inner, buf);
}

fn doctor_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    if loading(area, buf, theme, "DOCTOR", &app.doctor) {
        return;
    }
    let rows = app.doctor_rows();
    let problems = rows.iter().filter(|(_, finding)| !finding.ok).count();
    let block = panel(
        theme,
        Icon::Ok,
        "DOCTOR",
        &format!(
            "the host and each VM, problems first: {problems} problem{}",
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
    // Keep the chosen finding in view.
    let at = lines
        .iter()
        .position(|(_, selected)| *selected)
        .unwrap_or(0);
    let height = inner.height as usize;
    let start = (at + 2).saturating_sub(height);
    let shown: Vec<Line> = lines
        .into_iter()
        .skip(start)
        .take(height)
        .map(|(line, selected)| {
            if selected {
                Line::from(
                    std::iter::once(Span::styled(
                        format!("{} ", theme.icon(Icon::Pointer)),
                        theme.selected(),
                    ))
                    .chain(
                        line.spans
                            .into_iter()
                            .map(|span| span.style(theme.selected())),
                    )
                    .collect::<Vec<_>>(),
                )
            } else {
                Line::from(
                    std::iter::once(Span::styled("  ", theme.base()))
                        .chain(line.spans)
                        .collect::<Vec<_>>(),
                )
            }
        })
        .collect();
    Paragraph::new(shown).style(theme.base()).render(inner, buf);
}

fn logs_screen(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let filter = match &app.log_filter {
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
            // Newest at the bottom, scrolled back by `scroll`, wrapping long lines.
            let rows = wrapped_rows(&lines, inner.width);
            let height = inner.height as usize;
            let end = rows.saturating_sub(flow.scroll);
            let skip = end.saturating_sub(height);
            Paragraph::new(lines)
                .style(theme.base())
                .wrap(Wrap { trim: false })
                .scroll((skip as u16, 0))
                .render(inner, buf);
        }
        Dialog::Choice(choice) => {
            let rows = 3 + choice.text.len() as u16 + 2 * choice.choices.len() as u16 + 2;
            let rect = centred(area, width.min(100), rows.min(height));
            Clear.render(rect, buf);
            let block = panel(theme, Icon::Settings, &choice.title, "");
            let inner = block.inner(rect);
            block.render(rect, buf);
            let mut lines: Vec<Line> = choice
                .text
                .iter()
                .map(|text| widgets::line(theme, text.clone(), Tone::Normal))
                .collect();
            lines.push(Line::default());
            for (index, option) in choice.choices.iter().enumerate() {
                let style = if index == choice.cursor {
                    theme.selected()
                } else {
                    theme.value()
                };
                lines.push(Line::from(vec![Span::styled(
                    format!(" {}  {} ", option.key, option.label),
                    style,
                )]));
                lines.push(Line::from(Span::styled(
                    format!("    {}", option.detail),
                    theme.dim(),
                )));
            }
            lines.push(widgets::line(
                theme,
                " Esc  back, nothing changed",
                Tone::Dim,
            ));
            Paragraph::new(lines)
                .style(theme.base())
                .wrap(Wrap { trim: false })
                .render(inner, buf);
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

/// How many rows `lines` take wrapped at `width`.
fn wrapped_rows(lines: &[Line], width: u16) -> usize {
    let width = width.max(1) as usize;
    lines
        .iter()
        .map(|line| line.width().max(1).div_ceil(width))
        .sum()
}

fn help(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let rect = centred(area, 76, 22);
    Clear.render(rect, buf);
    let block = panel(theme, Icon::Settings, "KEYS", "Esc closes");
    let inner = block.inner(rect);
    block.render(rect, buf);
    let rows: [(&str, &str); 18] = [
        ("Tab, Shift+Tab", "next and previous screen"),
        ("1 to 6", "Dashboard, VM, Add, Remove, Doctor, Logs"),
        ("arrows, j k", "choose; Home, End, PgUp, PgDn"),
        ("Enter", "open, or run what is chosen"),
        ("Esc", "back, or close a dialog"),
        ("r", "read the screen again"),
        ("e", "the errors list"),
        ("t", "next colour theme: navy, light, terminal"),
        ("i", "Unicode or ASCII icons"),
        ("q, Ctrl+C", "quit"),
        ("", ""),
        ("VM screen", ""),
        ("s", "change priority, min_ram, max_ram, free_ram"),
        ("a", "attest it again"),
        ("x", "remove it"),
        ("d, l", "its doctor, its logs"),
        ("left, right", "the previous and next VM"),
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
    use crate::reports::{ListedVm, Problem, VmListReport, VmListSchema};
    use crate::tui::actions::Action;
    use crate::tui::app::{Choice, ChoiceDialog, ErrorEntry, FlowDialog};
    use crate::tui::jobs::{DoctorSection, Msg};
    use crate::tui::pool::tests::themes;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

    fn app() -> crate::tui::app::App {
        let (tx, _rx) = super::super::jobs::channel();
        let context = crate::context::Context {
            config: None,
            connect: Some("test:///default".to_owned()),
            build: "0.1.0 (build test)".to_owned(),
            via: "tui",
        };
        let mut app = crate::tui::app::App::new(
            context,
            themes()[0],
            tx,
            super::super::jobs::ReaderControl::new(),
        );
        app.on_msg(Msg::Pool(Ok(crate::tui::pool::tests::snapshot(3))));
        app.on_msg(Msg::Vm(
            "vm0".to_owned(),
            Box::new(Ok(crate::tui::vm::tests::snapshot())),
        ));
        app.on_msg(Msg::Details(
            "vm0".to_owned(),
            crate::tui::vm::tests::details(),
        ));
        app.on_msg(Msg::Inventory(Ok(VmListReport {
            schema: VmListSchema,
            version: 1,
            vms: vec![
                ListedVm {
                    vm_name: "newvm".to_owned(),
                    state: "running".to_owned(),
                    management: Management::NotManaged,
                    status: None,
                    detail: "the guest service hasn't published".to_owned(),
                    to_fix: vec!["add a 6144 MiB virtio-mem device".to_owned()],
                    problems: vec![],
                    guest: Published::No,
                    os_id: Some("http://debian.org/debian/13".to_owned()),
                },
                ListedVm {
                    vm_name: "blocked".to_owned(),
                    state: "shut off".to_owned(),
                    management: Management::NotManaged,
                    status: None,
                    detail: String::new(),
                    to_fix: vec![],
                    problems: vec![Problem {
                        problem: "more than one virtio-mem device".to_owned(),
                        fix: "remove the others".to_owned(),
                    }],
                    guest: Published::Unknown,
                    os_id: None,
                },
                ListedVm {
                    vm_name: "vm0".to_owned(),
                    state: "running".to_owned(),
                    management: Management::Managed,
                    status: None,
                    detail: "steady".to_owned(),
                    to_fix: vec![],
                    problems: vec![],
                    guest: Published::Yes { age_seconds: 1 },
                    os_id: None,
                },
                ListedVm {
                    vm_name: "oldvm".to_owned(),
                    state: "shut off".to_owned(),
                    management: Management::Parked,
                    status: None,
                    detail: "parked".to_owned(),
                    to_fix: vec![],
                    problems: vec![],
                    guest: Published::Unknown,
                    os_id: None,
                },
            ],
        })));
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
    fn every_screen_draws_at_every_size_in_every_theme() {
        let mut app = app();
        let expected = [
            (Screen::Dashboard, "POOL EVENTS"),
            (Screen::Vm, "VIRTIO-MEM DEVICE"),
            (Screen::Add, "newvm"),
            (Screen::Remove, "oldvm"),
            (Screen::Doctor, "drifted"),
            (Screen::Logs, "operator_change"),
        ];
        for theme in themes() {
            app.theme = theme;
            for (screen, needle) in expected {
                app.screen = screen;
                for (width, height) in SIZES {
                    let text = text(&app, width, height);
                    assert!(
                        text.contains(needle),
                        "{screen:?} at {width}x{height} lacks {needle}:\n{text}"
                    );
                    assert!(text.contains("RAMpire"), "{text}");
                    assert_eq!(text.lines().count(), height as usize, "{screen:?}\n{text}");
                }
            }
        }
    }

    #[test]
    fn the_add_screen_says_how_to_install_the_guest_service() {
        let mut app = app();
        app.screen = Screen::Add;
        let text = text(&app, 120, 40);
        assert!(text.contains("apt install rampire-guest"), "{text}");
        assert!(
            text.contains("rampire --connect test:///default vm add newvm --fix-xml"),
            "{text}"
        );
        app.on_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let text = self::text(&app, 120, 40);
        assert!(text.contains("remove the others"), "{text}");
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
            title: "Stop managing vm0".to_owned(),
            text: vec!["It keeps its memory.".to_owned()],
            choices: vec![Choice {
                key: 'k',
                label: "Keep its settings".to_owned(),
                detail: "parked".to_owned(),
                action: Action::Remove {
                    vm: "vm0".to_owned(),
                    forget: false,
                },
            }],
            cursor: 0,
        }));
        assert!(text(&app, 80, 24).contains("Keep its settings"));
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
        assert!(text(&app, 80, 24).contains("Shift+Tab"));
    }

    #[test]
    fn the_doctor_offers_to_run_a_fix_that_is_a_command() {
        let mut app = app();
        app.screen = Screen::Doctor;
        let text = text(&app, 120, 40);
        assert!(text.contains("[Enter runs it]"), "{text}");
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let Some(Dialog::Choice(choice)) = &app.dialog else {
            panic!("a dialog to confirm the fix");
        };
        assert_eq!(
            choice.choices[0].action,
            Action::Attest {
                vm: "vm0".to_owned(),
                restart: false
            }
        );
    }
}
