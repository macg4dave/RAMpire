//! The controller's journal, all of it or one VM's lines, newest at the
//! bottom; `/` finds a text and marks its lines, `n` and `N` go between
//! them, `y` copies the chosen line.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use super::doctor::loading;
use crate::tone::Tone;
use crate::tui::app::{App, Loaded};
use crate::tui::theme::Icon;
use crate::tui::widgets::{self, panel};

pub fn render(area: Rect, buf: &mut Buffer, app: &App, vm: Option<&str>) {
    let theme = &app.theme;
    let Loaded::Done(Ok(_)) = &app.logs.loaded else {
        loading(area, buf, theme, "LOGS", &app.logs.loaded);
        return;
    };
    let mut detail = match vm {
        Some(vm) => format!("the controller's journal, lines about {vm} (f: next VM)"),
        None => "the controller's journal, every line (f: one VM)".to_owned(),
    };
    let lines = app.log_lines();
    if let Some(find) = &app.logs.find {
        let found = lines.iter().filter(|line| app.found(line)).count();
        detail.push_str(&format!("; /{find}: {found} found"));
    }
    let block = panel(theme, Icon::Vm, "LOGS", &detail);
    let inner = block.inner(area);
    block.render(area, buf);
    if lines.is_empty() {
        Paragraph::new(widgets::line(theme, "no journal lines", Tone::Dim)).render(inner, buf);
        return;
    }
    let window = app.logs.scroll.window(lines.len(), inner.height as usize);
    let cursor = app.logs.scroll.cursor(lines.len());
    let marker = theme.icon(Icon::Pointer);
    let shown: Vec<Line> = window
        .filter_map(|index| lines.get(index).map(|line| (index, *line)))
        .map(|(index, line)| {
            let tone = if line.contains("event=operator_change") {
                Tone::Accent
            } else if line.contains("latched") || line.contains("error") || line.contains("refuse")
            {
                Tone::Bad
            } else if line.contains("Partial") || line.contains("warning") {
                Tone::Warn
            } else {
                Tone::Normal
            };
            let found = app.found(line);
            let style = if index == cursor {
                theme.selected()
            } else if found {
                theme.chip()
            } else {
                theme.tone(tone)
            };
            let text = widgets::fit(
                &compact_log_line(line),
                inner.width.saturating_sub(2) as usize,
            );
            Line::from(vec![
                Span::styled(
                    if found {
                        format!("{marker} ")
                    } else {
                        "  ".to_owned()
                    },
                    theme.accent(),
                ),
                Span::styled(text, style),
            ])
        })
        .collect();
    Paragraph::new(shown).style(theme.card()).render(inner, buf);
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
