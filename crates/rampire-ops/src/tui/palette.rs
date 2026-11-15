//! The command palette: type a few letters of anything the TUI can do,
//! for the chosen VM or anywhere, each command with its key. Its commands
//! are the chosen VM's menu, every key of what is shown, and the themes.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::app::Run;
use super::input::{History, Input};
use super::scroll::Scroll;
use super::theme::{Icon, Palette as Colours, Theme};
use super::widgets::{self, raised_panel};
use crate::tone::Tone;

/// One command in the palette.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The group it is listed under: the VM's name, a view, Theme.
    pub group: String,
    pub label: String,
    /// The key that does the same, if there is one.
    pub key: String,
    pub run: Run,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandPalette {
    pub input: Input,
    pub scroll: Scroll,
    pub items: Vec<Item>,
    /// The theme before a theme was previewed, put back on Esc.
    pub theme_before: Colours,
}

impl CommandPalette {
    pub fn new(items: Vec<Item>, theme_before: Colours) -> Self {
        Self {
            input: Input::default(),
            scroll: Scroll::default(),
            items,
            theme_before,
        }
    }

    /// The items the query matches, best first; all of them, in their
    /// groups, before anything is typed.
    pub fn matches(&self) -> Vec<&Item> {
        let query = self.input.value();
        if query.trim().is_empty() {
            return self.items.iter().collect();
        }
        let mut scored: Vec<(i64, usize, &Item)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let text = format!("{} {}", item.group, item.label);
                fuzzy_score(query, &text).map(|score| (score, index, item))
            })
            .collect();
        scored.sort_by_key(|(score, index, _)| (-score, *index));
        scored.into_iter().map(|(_, _, item)| item).collect()
    }

    pub fn chosen(&self) -> Option<&Item> {
        let matches = self.matches();
        matches.get(self.scroll.cursor(matches.len())).copied()
    }

    /// A search box near the top, and what matches it, grouped, each with
    /// its key.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, history: &History) {
        let matches = self.matches();
        let width = area.width.saturating_sub(8).min(72);
        let height = (matches.len() as u16 + 5)
            .min(area.height.saturating_sub(2))
            .max(6);
        let [rect] = Layout::horizontal([Constraint::Length(width)])
            .flex(Flex::Center)
            .areas(area);
        let rect = Rect {
            y: area.y + 1,
            height,
            ..rect
        };
        Clear.render(rect, buf);
        let block = raised_panel(
            theme,
            Icon::Pointer,
            "COMMANDS",
            "type to find, Enter runs, Esc closes",
        );
        let inner = block.inner(rect);
        block.render(rect, buf);
        let mut spans = vec![Span::styled(
            format!("{} ", theme.icon(Icon::Pointer)),
            theme.accent(),
        )];
        spans.extend(
            self.input
                .line(theme, history, inner.width.saturating_sub(2) as usize)
                .spans,
        );
        let mut lines = vec![Line::from(spans), Line::default()];
        let rows = inner.height.saturating_sub(2) as usize;
        let window = self.scroll.window(matches.len(), rows);
        let cursor = self.scroll.cursor(matches.len());
        let label_width = inner.width.saturating_sub(26) as usize;
        let mut group = String::new();
        for index in window {
            let Some(item) = matches.get(index) else {
                continue;
            };
            let style = if index == cursor {
                theme.selected()
            } else {
                theme.text()
            };
            let mut spans = vec![Span::styled(
                if item.group != group {
                    group.clone_from(&item.group);
                    format!(" {:<10}", widgets::fit(&item.group, 10))
                } else {
                    " ".repeat(11)
                },
                theme.dim(),
            )];
            spans.push(Span::styled(
                format!(" {:<label_width$} ", widgets::fit(&item.label, label_width)),
                style,
            ));
            if !item.key.is_empty() {
                spans.push(Span::styled(format!(" {} ", item.key), theme.key()));
            }
            lines.push(Line::from(spans));
        }
        if matches.is_empty() {
            lines.push(widgets::line(theme, "  nothing matches", Tone::Dim));
        }
        Paragraph::new(lines)
            .style(theme.raised())
            .render(inner, buf);
    }
}

/// How well `query` matches `text`: its letters in order, case aside,
/// scoring letters that follow each other or start a word higher. `None`
/// when they aren't all there.
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    let text: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i64;
    let mut at = 0usize;
    let mut last: Option<usize> = None;
    for wanted in query.to_lowercase().chars().filter(|c| !c.is_whitespace()) {
        let found = (at..text.len()).find(|&index| text[index] == wanted)?;
        score += 1;
        if last.is_some_and(|last| last + 1 == found) {
            score += 5;
        }
        if found == 0 || !text[found - 1].is_alphanumeric() {
            score += 3;
        }
        last = Some(found);
        at = found + 1;
    }
    // Shorter texts with the same letters come first.
    Some(score * 100 - text.len() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_matching_prefers_letters_together_and_word_starts() {
        assert!(fuzzy_score("dr", "Doctor").is_some());
        assert!(fuzzy_score("xyz", "Doctor").is_none());
        let together = fuzzy_score("log", "Logs").unwrap();
        let apart = fuzzy_score("log", "Leave our garden").unwrap();
        assert!(together > apart);
    }
}
