//! Drawing the TUI: the header and status bar around the view, then what
//! is open over it, from the bottom up as Yazi's root draws its layers, so
//! what takes the keys is always on top.

pub mod doctor;
pub mod logs;
pub mod main;

use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget, Wrap};
use ratatui::Frame;

use super::app::{App, Overlay, View};
use super::dialog::Dialog;
use super::jobs::Question;
use super::keymap::{self, Layer};
use super::theme::{Icon, Theme};
use super::widgets::{self, panel, raised_panel};
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
    let now = Instant::now();
    Clear.render(area, buf);
    buf.set_style(area, theme.base());
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        let text = format!(
            "RAMpire needs a terminal of at least {MIN_WIDTH}x{MIN_HEIGHT}; this one is {}x{}. Make it larger, or press q to quit.",
            area.width, area.height
        );
        Paragraph::new(text)
            .style(theme.card())
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
        View::Main => main::render(body, buf, app),
        View::Details(vm) => details(body, buf, app, vm),
        View::Doctor(vm) => doctor::render(body, buf, app, vm.as_deref()),
        View::Logs(vm) => logs::render(body, buf, app, vm.as_deref()),
    }
    status_bar(status, buf, app);
    match &app.overlay {
        Some(Overlay::Tasks) => super::tasks::render_list(body, buf, theme, &app.tasks, now),
        Some(Overlay::Task { id, .. }) => {
            if let Some(task) = app.tasks.get(*id) {
                super::tasks::render_task(body, buf, theme, &app.history, task, now);
            }
        }
        Some(Overlay::Spot(spot)) => spot.render(body, buf, app),
        Some(Overlay::Errors(scroll)) => errors(body, buf, app, scroll),
        None => {}
    }
    match &app.dialog {
        Some(Dialog::Confirm(confirm)) => confirm.render(body, buf, theme),
        Some(Dialog::Form(form)) => form.render(body, buf, theme, &app.history, &app.context),
        None => {}
    }
    if app.prompt.is_some() {
        prompt(body, buf, app);
    }
    if let Some(palette) = &app.palette {
        palette.render(body, buf, theme, &app.history);
    }
    if let Some(which) = &app.which {
        let row = match app.view {
            View::Main => main::row_y(body, app),
            _ => None,
        };
        which.render(body, buf, theme, row);
    }
    if let Some(help) = &app.help {
        help.render(body, buf, theme, &app.history);
    }
    app.notices.render(area, buf, theme, now);
}

fn header_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    let bar = theme.bar();
    let bold = ratatui::style::Modifier::BOLD;
    let (state, tone) = match &app.controller {
        Some(unit) if unit.active_state == "active" => (
            format!("{} {}", theme.icon(Icon::Running), unit.sub_state),
            Tone::Good,
        ),
        Some(unit) => (
            format!("{} {}", theme.icon(Icon::Problem), unit.active_state),
            Tone::Bad,
        ),
        None => ("...".to_owned(), Tone::Dim),
    };
    // RAM in white, pire in the cape's magenta, as the logo writes it.
    let left = Line::from(vec![
        Span::styled(" ", bar),
        Span::styled("RAM", bar.patch(theme.value())),
        Span::styled("pire", bar.patch(theme.accent())),
        Span::styled("   ", bar),
        Span::styled(
            format!("{} {}", theme.icon(Icon::Host), short_host()),
            bar.add_modifier(bold),
        ),
        Span::styled("   controller ", bar),
        Span::styled(state, bar.patch(theme.tone(tone))),
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
    buf.set_style(area, bar);
    Paragraph::new(left).style(bar).render(area, buf);
    let width = right.chars().count() as u16;
    if area.width > width + 50 {
        buf.set_string(area.right() - width, area.y, right, bar.patch(theme.dim()));
    }
}

/// The host's name up to its first dot.
fn short_host() -> String {
    let name = crate::dashboard::host_name();
    name.split('.').next().unwrap_or(&name).to_owned()
}

/// The keys for what takes them, from the key table; on the right, the
/// tasks' gauge, the marks, the filter and the errors.
fn status_bar(area: Rect, buf: &mut Buffer, app: &App) {
    let theme = &app.theme;
    buf.set_style(area, theme.bar());
    let layer = app.layer();
    let typing = layer == Layer::Task
        && matches!(&app.overlay, Some(Overlay::Task { id, .. })
            if app.tasks.get(*id).and_then(|task| task.pending.as_ref())
                .is_some_and(|pending| matches!(pending.question, Question::Text(_))));
    let hints: Vec<(String, &str)> = if typing {
        vec![
            ("type".to_owned(), "the answer"),
            ("Enter".to_owned(), "answer"),
            ("Esc".to_owned(), "later"),
        ]
    } else if layer == Layer::Which {
        vec![("Esc".to_owned(), "back")]
    } else {
        keymap::hints(layer)
    };
    let mut spans = vec![Span::styled(" ", theme.bar())];
    for (key, hint) in &hints {
        spans.push(Span::styled(format!(" {key} "), theme.key()));
        spans.push(Span::styled(format!(" {hint}  "), theme.bar()));
    }
    let mut right: Vec<Span> = Vec::new();
    if let Some(gauge) = super::tasks::gauge(theme, &app.tasks, 24) {
        right.extend(gauge.spans);
    }
    if !app.list.marks.is_empty() {
        right.push(Span::styled(
            format!(" {} marked ", app.list.marks.len()),
            theme.bar().patch(theme.accent()),
        ));
    }
    if let Some(filter) = &app.list.filter {
        right.push(Span::styled(
            format!(" /{filter} "),
            theme.bar().patch(theme.accent()),
        ));
    }
    if !app.errors.is_empty() {
        right.push(Span::styled(
            format!(
                " {} {} error{} (e) ",
                theme.icon(Icon::Problem),
                app.errors.len(),
                if app.errors.len() == 1 { "" } else { "s" }
            ),
            theme.bar().add_modifier(ratatui::style::Modifier::BOLD),
        ));
    }
    let right = Line::from(right);
    let right_width = right.width() as u16;
    Paragraph::new(Line::from(spans))
        .style(theme.bar())
        .render(area, buf);
    if right_width > 0 && area.width > right_width {
        let place = Rect {
            x: area.right() - right_width,
            width: right_width,
            ..area
        };
        Clear.render(place, buf);
        Paragraph::new(right).style(theme.bar()).render(place, buf);
    }
}

/// A panel holding one message.
pub fn message(area: Rect, buf: &mut Buffer, theme: &Theme, title: &str, text: &str, tone: Tone) {
    let block = panel(theme, Icon::Vm, title, "");
    let inner = block.inner(area);
    block.render(area, buf);
    Paragraph::new(text.to_owned())
        .style(theme.tone(tone))
        .wrap(Wrap { trim: true })
        .render(inner, buf);
}

/// One managed VM live: the `watch` view.
fn details(area: Rect, buf: &mut Buffer, app: &App, vm: &str) {
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

/// A filter or find being typed: a field near the top, as Yazi's.
fn prompt(area: Rect, buf: &mut Buffer, app: &App) {
    let Some(prompt) = &app.prompt else {
        return;
    };
    let theme = &app.theme;
    let (title, detail) = match app.view {
        View::Logs(_) => (
            "FIND",
            "the lines with it are marked; n and N go between them",
        ),
        View::Doctor(_) => ("FILTER", "the findings with it"),
        _ => ("FILTER", "the VMs whose name has it; Tab completes a name"),
    };
    let width = area.width.saturating_sub(4).min(80);
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + 1,
        width,
        height: 3,
    };
    Clear.render(rect, buf);
    let block = raised_panel(theme, Icon::Pointer, title, detail);
    let inner = block.inner(rect);
    block.render(rect, buf);
    Paragraph::new(prompt.input.line(theme, &app.history, inner.width as usize))
        .style(theme.raised())
        .render(inner, buf);
}

/// This session's errors, newest last, the chosen one marked.
fn errors(area: Rect, buf: &mut Buffer, app: &App, scroll: &super::scroll::Scroll) {
    let theme = &app.theme;
    let rect = widgets::centred(
        area,
        area.width.saturating_sub(4),
        area.height.saturating_sub(2),
    );
    Clear.render(rect, buf);
    let block = raised_panel(
        theme,
        Icon::Problem,
        "ERRORS",
        "this session's, newest last; y copies one",
    );
    let inner = block.inner(rect);
    block.render(rect, buf);
    let window = scroll.window(app.errors.len(), (inner.height as usize / 2).max(1));
    let cursor = scroll.cursor(app.errors.len());
    let mut lines = Vec::new();
    for index in window {
        let Some(entry) = app.errors.get(index) else {
            continue;
        };
        let what = if index == cursor {
            theme.selected()
        } else {
            theme.value()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", entry.time), theme.dim()),
            Span::styled(entry.what.clone(), what),
        ]));
        lines.push(widgets::line(
            theme,
            format!(
                "   {}",
                widgets::fit(&entry.error, inner.width.saturating_sub(4) as usize)
            ),
            Tone::Bad,
        ));
    }
    Paragraph::new(lines)
        .style(theme.raised())
        .render(inner, buf);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::actions::Action;
    use crate::tui::app::ErrorEntry;
    use crate::tui::dialog::{Confirm, OnYes};
    use crate::tui::jobs::{DoctorSection, FlowEvent, Msg};
    use crate::tui::pool::tests::themes;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    const SIZES: [(u16, u16); 3] = [(80, 24), (120, 40), (200, 60)];

    pub(crate) fn app() -> App {
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
                        "recorded setup",
                        "its definition changed since RAMpire last checked this VM",
                        "rampire apply",
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

    pub(crate) fn text(app: &App, width: u16, height: u16) -> String {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, app);
        widgets::plain_text(&buf)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn every_view_draws_at_every_size_in_every_theme() {
        let mut app = app();
        let expected = [
            (View::Main, "EVENTS"),
            (View::Details("vm0".to_owned()), "DEVICE"),
            (View::Doctor(None), "definition changed"),
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
    fn the_main_screen_lists_every_vm_and_previews_the_chosen_one_when_wide() {
        let mut app = app();
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            for needle in [
                "vm0",
                "grow +1024M",
                "oldvm",
                "unfinished",
                "finish",
                "newvm",
                "not managed",
                "no guest service",
                "HISTORY",
            ] {
                assert!(
                    text.contains(needle),
                    "{width}x{height} lacks {needle}:\n{text}"
                );
            }
        }
        // Wide and tall, the chosen VM's details are beside the list.
        let text = text(&app, 200, 60);
        assert!(text.contains("DEVICE") && text.contains("GUEST"), "{text}");
        // The last columns change with m; marks show.
        app.list.linemode = crate::tui::list::Linemode::Limits;
        app.list.marks.insert("vm0".to_owned());
        let text = super::tests::text(&app, 120, 40);
        assert!(text.contains("MAX") && text.contains("1 marked"), "{text}");
    }

    #[test]
    fn a_vms_menu_draws_beside_its_row_with_its_keys_and_commands() {
        let mut app = app();
        press(&mut app, KeyCode::Enter);
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            for needle in [
                "vm0 (managed)",
                "v  Live details",
                "s  Settings",
                "o  Doctor: check everything about it",
                "Remove it",
                "same as: rampire",
            ] {
                assert!(
                    text.contains(needle),
                    "{width}x{height} lacks {needle}:\n{text}"
                );
            }
        }
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::End);
        press(&mut app, KeyCode::Enter);
        let text = text(&app, 120, 40);
        assert!(text.contains("Add it"), "{text}");
    }

    #[test]
    fn every_layer_draws_over_the_screen_at_every_size() {
        let mut app = app();
        let id = app
            .tasks
            .add(Action::add("newvm"), "rampire vm add newvm".to_owned());
        app.tasks.start_next(Instant::now());
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Line("--- saved definition".to_owned()),
        ));
        let (reply, _answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(
                Question::Confirm("Change newvm's saved definition like this?".to_owned()),
                reply,
            ),
        ));
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            assert!(text.contains("rampire vm add newvm"), "{text}");
            assert!(text.contains("like this? [y/N]"), "{text}");
            assert!(
                text.contains("y  Yes") && text.contains("1 asks (w)"),
                "{text}"
            );
        }
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('w'));
        assert!(text(&app, 80, 24).contains("TASKS"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Tab);
        let facts = text(&app, 80, 24);
        assert!(
            facts.contains("vm0 facts") && facts.contains("Priority"),
            "{facts}"
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('?'));
        assert!(text(&app, 80, 24).contains("the chosen VM's menu"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('/'));
        assert!(text(&app, 80, 24).contains("FILTER"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('g'));
        assert!(text(&app, 80, 24).contains("to the top"));
        press(&mut app, KeyCode::Esc);
        app.open_palette();
        app.notify("done: rampire set vm0 priority=110", Tone::Good);
        // As they show once they have slid in.
        for message in &mut app.notices.messages {
            message.shown -= std::time::Duration::from_secs(1);
        }
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            assert!(text.contains("COMMANDS"), "{text}");
            assert!(text.contains("Live details"), "{text}");
            assert!(
                text.contains("done: rampire set"),
                "{width}x{height}\n{text}"
            );
        }
        app.palette = None;
        app.dialog = Some(Dialog::Confirm(Confirm::new(
            "Run the fix",
            vec!["This runs:".to_owned()],
            vec!["rampire apply".to_owned()],
            OnYes::Run(vec![Action::Apply]),
        )));
        assert!(text(&app, 80, 24).contains("rampire apply"));
        app.dialog = None;
        app.errors.push(ErrorEntry {
            time: "10:00:00".to_owned(),
            what: "rampire apply".to_owned(),
            error: "the draft did not pass its checks".to_owned(),
        });
        assert!(text(&app, 80, 24).contains("1 error (e)"));
        press(&mut app, KeyCode::Char('e'));
        assert!(text(&app, 80, 24).contains("the draft did not pass its checks"));
    }

    #[test]
    fn a_long_task_shows_its_newest_line_whatever_wraps() {
        let mut app = app();
        let id = app.tasks.add(Action::Apply, "rampire apply".to_owned());
        app.tasks.start_next(Instant::now());
        for _ in 0..40 {
            app.on_msg(Msg::Task(id, FlowEvent::Line("word ".repeat(30))));
        }
        let (reply, _answer) = std::sync::mpsc::channel();
        app.on_msg(Msg::Task(
            id,
            FlowEvent::Ask(
                Question::Confirm("Apply this and restart rampire.service?".to_owned()),
                reply,
            ),
        ));
        for (width, height) in SIZES {
            let text = text(&app, width, height);
            assert!(text.contains("restart rampire.service? [y/N]"), "{text}");
        }
    }

    #[test]
    fn a_small_terminal_says_the_size_it_needs() {
        let app = app();
        let text = text(&app, 79, 24);
        assert!(text.contains("at least 80x24"), "{text}");
        assert!(text.contains("79x24"), "{text}");
    }

    #[test]
    fn the_doctor_offers_to_run_a_fix_that_is_a_command() {
        let mut app = app();
        app.view = View::Doctor(None);
        let text = text(&app, 120, 40);
        assert!(text.contains("[Enter runs it"), "{text}");
        press(&mut app, KeyCode::Enter);
        let Some(Dialog::Confirm(confirm)) = &app.dialog else {
            panic!("a question before the fix runs");
        };
        assert_eq!(confirm.on_yes, OnYes::Run(vec![Action::Apply]));
        press(&mut app, KeyCode::Esc);
        // Problems only, and a filter.
        press(&mut app, KeyCode::Char('p'));
        let text = super::tests::text(&app, 120, 40);
        assert!(
            text.contains("definition changed") && !text.contains("libvirt:"),
            "{text}"
        );
    }
}
