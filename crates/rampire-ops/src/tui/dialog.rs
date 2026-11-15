//! The dialogs: a question to confirm, laid out as Yazi's are (what it is
//! about, a list of exactly what it will do, then No and Yes), and the
//! settings form for one VM or several.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::actions::Action;
use super::input::{History, Input};
use super::scroll::Scroll;
use super::theme::{Icon, Theme};
use super::widgets::{self, raised_panel};
use crate::tone::Tone;

pub enum Dialog {
    Confirm(Confirm),
    Form(Form),
}

/// What Yes does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OnYes {
    Quit,
    /// Start these, each a task.
    Run(Vec<Action>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirm {
    pub title: String,
    pub body: Vec<String>,
    /// What it will do, line by line.
    pub list: Vec<String>,
    pub scroll: Scroll,
    /// The Yes button is chosen; No is the default.
    pub yes: bool,
    pub on_yes: OnYes,
}

impl Confirm {
    pub fn new(
        title: impl Into<String>,
        body: Vec<String>,
        list: Vec<String>,
        on_yes: OnYes,
    ) -> Self {
        Self {
            title: title.into(),
            body,
            list,
            scroll: Scroll::default(),
            yes: false,
            on_yes,
        }
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let width = area.width.saturating_sub(8).min(80);
        let inner_width = width.saturating_sub(4) as usize;
        let body: Vec<Line<'static>> = self
            .body
            .iter()
            .flat_map(|text| {
                widgets::wrap(
                    widgets::line(theme, text.clone(), Tone::Normal),
                    inner_width,
                )
            })
            .collect();
        let list: Vec<Line<'static>> = self
            .list
            .iter()
            .flat_map(|text| {
                widgets::wrap(widgets::line(theme, text.clone(), Tone::Value), inner_width)
            })
            .collect();
        let divider = u16::from(!body.is_empty() && !list.is_empty());
        let wanted = body.len() as u16 + divider + list.len() as u16 + 2 /* gap, buttons */ + 2;
        let rect = widgets::centred(area, width, wanted.min(area.height.saturating_sub(2)));
        Clear.render(rect, buf);
        let block = raised_panel(theme, Icon::Problem, &self.title, "");
        let inner = block.inner(rect);
        block.render(rect, buf);
        let [body_area, divider_area, list_area, _, buttons_area] = Layout::vertical([
            Constraint::Length(body.len() as u16),
            Constraint::Length(divider),
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        Paragraph::new(body)
            .style(theme.raised())
            .render(body_area, buf);
        if divider > 0 {
            let rule = if theme.icons.unicode { "─" } else { "-" };
            Paragraph::new(Line::styled(
                rule.repeat(divider_area.width as usize),
                theme.frame(),
            ))
            .render(divider_area, buf);
        }
        let window = self.scroll.window(list.len(), list_area.height as usize);
        Paragraph::new(list[window].to_vec())
            .style(theme.raised())
            .render(list_area, buf);
        Paragraph::new(widgets::buttons(theme, buttons_area.width, self.yes))
            .render(buttons_area, buf);
    }
}

/// One setting in the form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub key: &'static str,
    pub input: Input,
    /// Its value now, for one VM; for several, none: left empty, each
    /// keeps its own.
    pub original: Option<String>,
}

impl Field {
    pub fn new(key: &'static str, original: Option<String>) -> Self {
        let input = Input::new(original.as_deref().unwrap_or(""))
            .remembering(format!("setting {key}"))
            .offering(match key {
                "priority" => vec!["100".to_owned(), "150".to_owned(), "50".to_owned()],
                _ => ["1GiB", "2GiB", "4GiB", "8GiB", "16GiB", "512MiB"]
                    .iter()
                    .map(|size| (*size).to_owned())
                    .collect(),
            });
        Self {
            key,
            input,
            original,
        }
    }

    fn changed(&self) -> bool {
        let value = self.input.value().trim();
        match &self.original {
            Some(original) => value != original,
            None => !value.is_empty(),
        }
    }
}

/// The settings form, for the VMs it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub vms: Vec<String>,
    pub fields: Vec<Field>,
    pub cursor: usize,
}

impl Form {
    /// The changes, as `KEY=VALUE`.
    pub fn changes(&self) -> Vec<String> {
        self.fields
            .iter()
            .filter(|field| field.changed())
            .map(|field| format!("{}={}", field.key, field.input.value().trim()))
            .collect()
    }

    /// The commands Enter runs.
    pub fn actions(&self) -> Vec<Action> {
        let changes = self.changes();
        if changes.is_empty() {
            return Vec::new();
        }
        self.vms
            .iter()
            .map(|vm| Action::Set {
                target: vm.clone(),
                changes: changes.clone(),
            })
            .collect()
    }

    pub fn render(
        &self,
        area: Rect,
        buf: &mut Buffer,
        theme: &Theme,
        history: &History,
        context: &crate::context::Context,
    ) {
        let height = self.fields.len() as u16 + 6 + self.vms.len().min(4) as u16;
        let rect = widgets::centred(
            area,
            area.width.saturating_sub(4).min(76),
            height.min(area.height.saturating_sub(2)),
        );
        Clear.render(rect, buf);
        let title = match self.vms.as_slice() {
            [vm] => format!("{vm} settings"),
            vms => format!("settings for {} VMs", vms.len()),
        };
        let block = raised_panel(theme, Icon::Settings, &title, "applied when saved");
        let inner = block.inner(rect);
        block.render(rect, buf);
        let mut lines = Vec::new();
        for (index, field) in self.fields.iter().enumerate() {
            let chosen = index == self.cursor;
            let value = if chosen {
                field.input.line(theme, history, 18)
            } else {
                Line::styled(format!("{:<18}", field.input.value()), theme.value())
            };
            let mut spans = vec![Span::styled(
                format!(" {:<10} ", field.key),
                if chosen {
                    theme.selected()
                } else {
                    theme.text()
                },
            )];
            spans.extend(value.spans);
            spans.push(Span::styled(
                match (&field.original, field.changed()) {
                    (Some(original), true) => format!("  (was {original})"),
                    (None, false) => "  (each keeps its own)".to_owned(),
                    _ => String::new(),
                },
                theme.dim(),
            ));
            lines.push(Line::from(spans));
        }
        lines.push(Line::default());
        let actions = self.actions();
        if actions.is_empty() {
            lines.push(widgets::line(theme, "no changes yet", Tone::Info));
        } else {
            lines.push(widgets::line(theme, "Enter runs:", Tone::Info));
            for action in actions.iter().take(4) {
                lines.push(widgets::line(
                    theme,
                    format!("  {}", action.command_line(context)),
                    Tone::Info,
                ));
            }
        }
        lines.push(widgets::line(
            theme,
            "sizes like 4GiB or 512MiB; Up recalls earlier values, Tab takes the suggestion",
            Tone::Dim,
        ));
        Paragraph::new(lines)
            .style(theme.raised())
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_changes_only_what_was_edited() {
        let mut form = Form {
            vms: vec!["web".to_owned()],
            fields: vec![
                Field::new("priority", Some("100".to_owned())),
                Field::new("max_ram", Some("8GiB".to_owned())),
            ],
            cursor: 0,
        };
        form.fields[0].input.set("150");
        assert_eq!(form.changes(), ["priority=150"]);
        // For several VMs, a field left empty leaves each VM's own value.
        form.vms.push("db".to_owned());
        form.fields[1] = Field::new("max_ram", None);
        assert_eq!(form.actions().len(), 2);
        assert_eq!(form.changes(), ["priority=150"]);
    }
}
