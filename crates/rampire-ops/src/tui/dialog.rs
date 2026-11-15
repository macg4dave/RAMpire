//! The dialogs: a question to confirm, laid out as Yazi's are (what it is
//! about, a list of exactly what it will do, then No and Yes), and the
//! settings form ([`super::form`]).

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::actions::Action;
use super::form::Form;
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
