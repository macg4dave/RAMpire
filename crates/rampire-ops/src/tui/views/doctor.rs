//! The doctor: the host's findings and each VM's, problems first, a fix
//! that is a `rampire` command run with Enter; filtered with `/`, problems
//! alone with `p`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::message;
use crate::tone::Tone;
use crate::tui::app::{App, Loaded};
use crate::tui::theme::{Icon, Theme};
use crate::tui::widgets::{self, panel};

/// A panel saying it is reading, or why it couldn't; whether it did.
pub fn loading<T>(
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

pub fn render(area: Rect, buf: &mut Buffer, app: &App, vm: Option<&str>) {
    let theme = &app.theme;
    let title = match vm {
        Some(vm) => format!("DOCTOR {vm}"),
        None => "DOCTOR".to_owned(),
    };
    if loading(area, buf, theme, &title, &app.doctor.loaded) {
        return;
    }
    let rows = app.doctor_rows();
    let problems = rows.iter().filter(|(_, finding)| !finding.ok).count();
    let mut detail = format!(
        "{}, problems first: {problems} problem{}",
        if vm.is_some() {
            "this VM"
        } else {
            "the host and each VM"
        },
        if problems == 1 { "" } else { "s" }
    );
    if app.doctor.problems_only {
        detail.push_str("; problems only (p: all)");
    }
    if let Some(filter) = &app.doctor.filter {
        detail.push_str(&format!("; /{filter}"));
    }
    let block = panel(theme, Icon::Ok, &title, &detail);
    let inner = block.inner(area);
    block.render(area, buf);
    if rows.is_empty() {
        Paragraph::new(widgets::line(
            theme,
            "no finding matches: Esc clears the filter, p shows all",
            Tone::Dim,
        ))
        .render(inner, buf);
        return;
    }
    // Recorded for the page keys; the rows drawn are wrapped below.
    app.doctor
        .scroll
        .window(rows.len(), inner.height as usize / 2);
    let cursor = app.doctor.scroll.cursor(rows.len());
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
            let runnable = crate::tui::actions::from_fix(fix).is_some();
            lines.push((
                Line::from(Span::styled(
                    format!(
                        "      fix{}: {fix}",
                        if runnable && selected {
                            " [Enter runs it, y copies it]"
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
    let mut wrapped: Vec<(Line, bool)> = Vec::new();
    for (line, selected) in lines {
        let prefix = if selected {
            Span::styled(format!("{} ", theme.icon(Icon::Pointer)), theme.selected())
        } else {
            Span::styled("  ", theme.card())
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
        for row in widgets::wrap(Line::from(spans), inner.width as usize) {
            wrapped.push((row, selected));
        }
    }
    // Keep the chosen finding, and the fix beneath it, in view.
    let last = wrapped
        .iter()
        .rposition(|(_, selected)| *selected)
        .unwrap_or(0);
    let height = inner.height as usize;
    let start = (last + 3).saturating_sub(height);
    let shown: Vec<Line> = wrapped
        .into_iter()
        .skip(start)
        .take(height)
        .map(|(line, _)| line)
        .collect();
    Paragraph::new(shown).style(theme.card()).render(inner, buf);
}
