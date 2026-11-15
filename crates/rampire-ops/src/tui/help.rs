// Copyright (c) 2023 - sxyazi
// Copyright (c) 2026 macg4dave
// SPDX-License-Identifier: MIT
//
// Adapted from Yazi (https://github.com/sxyazi/yazi) at commit
// d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: yazi-core/src/help/help.rs
// (`filter_chords`: the order in which typed text finds keys) and
// yazi-fm/src/help/help.rs (a titled box with a filter field over a divider
// and the keys beneath). Changed for RAMpire: it lists RAMpire's key table
// for what was open, Enter does the chosen key's command, and it is drawn
// in RAMpire's theme. The MIT license is in LICENSES/MIT.txt;
// THIRD_PARTY_NOTICES.md records it.

//! The keys for what is open, found by typing: a key itself first, then a
//! word of what it does.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::input::{History, Input};
use super::keymap::{self, Binding, Layer};
use super::scroll::Scroll;
use super::theme::{Icon, Theme};
use super::widgets::{self, raised_panel};
use crate::tone::Tone;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Help {
    /// What was open when the help was.
    pub layer: Layer,
    pub input: Input,
    pub scroll: Scroll,
}

impl Help {
    pub fn new(layer: Layer) -> Self {
        Self {
            layer,
            input: Input::default(),
            scroll: Scroll::default(),
        }
    }

    /// The keys shown: all of the layer's, or those the filter finds.
    pub fn bindings(&self) -> Vec<&'static Binding> {
        let all: Vec<&'static Binding> = keymap::bindings(self.layer).collect();
        let typed = self.input.value();
        if typed.is_empty() {
            all
        } else {
            filter(&all, typed)
        }
    }

    pub fn chosen(&self) -> Option<&'static Binding> {
        let bindings = self.bindings();
        bindings.get(self.scroll.cursor(bindings.len())).copied()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, history: &History) {
        let rect = widgets::centred(
            area,
            area.width.saturating_sub(8).min(90),
            area.height.saturating_sub(2),
        );
        Clear.render(rect, buf);
        let block = raised_panel(
            theme,
            Icon::Settings,
            "KEYS",
            &format!("for {}; type to find one", self.layer.name()),
        );
        let inner = block.inner(rect);
        block.render(rect, buf);
        let [field, divider, list] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(inner);
        let mut spans = vec![Span::styled(
            format!("{} ", theme.icon(Icon::Pointer)),
            theme.accent(),
        )];
        spans.extend(
            self.input
                .line(theme, history, field.width.saturating_sub(2) as usize)
                .spans,
        );
        Paragraph::new(Line::from(spans))
            .style(theme.raised())
            .render(field, buf);
        let rule = if theme.icons.unicode { "─" } else { "-" };
        Paragraph::new(Line::styled(
            rule.repeat(divider.width as usize),
            theme.frame(),
        ))
        .render(divider, buf);
        let bindings = self.bindings();
        if bindings.is_empty() {
            Paragraph::new(widgets::line(theme, "no key matches", Tone::Dim)).render(list, buf);
            return;
        }
        let window = self.scroll.window(bindings.len(), list.height as usize);
        let cursor = self.scroll.cursor(bindings.len());
        let lines: Vec<Line> = window
            .filter_map(|index| bindings.get(index).map(|binding| (index, binding)))
            .map(|(index, binding)| {
                let style = if index == cursor {
                    theme.selected()
                } else {
                    theme.text()
                };
                Line::from(vec![
                    Span::styled(format!(" {:<12}", binding.label()), theme.value()),
                    Span::styled(binding.desc.to_owned(), style),
                ])
            })
            .collect();
        Paragraph::new(lines)
            .style(theme.raised())
            .render(list, buf);
    }
}

/// The bindings `typed` finds, best first: those whose keys start with it
/// as typed, then case aside, then whose keys hold it, then whose
/// description does.
fn filter(bindings: &[&'static Binding], typed: &str) -> Vec<&'static Binding> {
    let lowercased = typed.to_lowercase();
    let mut exact = vec![];
    let mut icase = vec![];
    let mut contains = vec![];
    let mut desc = vec![];
    for binding in bindings {
        let on = binding.label();
        if on.starts_with(typed) {
            exact.push(*binding);
            continue;
        }
        let on = on.to_lowercase();
        if on.starts_with(&lowercased) {
            icase.push(*binding);
        } else if on.contains(&lowercased) {
            contains.push(*binding);
        } else if binding.desc.to_lowercase().contains(&lowercased) {
            desc.push(*binding);
        }
    }
    exact.extend(icase);
    exact.extend(contains);
    exact.extend(desc);
    exact
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::Cmd;

    #[test]
    fn typing_finds_a_key_by_itself_before_its_description() {
        let mut help = Help::new(Layer::List);
        assert!(help.bindings().len() > 30);
        help.input.set("q");
        assert_eq!(help.bindings()[0].cmd, Cmd::Quit);
        help.input.set("doctor");
        let found = help.bindings();
        assert!(found.iter().all(|binding| binding.desc.contains("doctor")));
        assert!(found.iter().any(|binding| binding.cmd == Cmd::Doctor));
        help.input.set("zzz");
        assert!(help.bindings().is_empty());
    }
}
