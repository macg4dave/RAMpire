// Copyright (c) 2023 - sxyazi
// Copyright (c) 2026 macg4dave
// SPDX-License-Identifier: MIT
//
// `History` is adapted from Yazi (https://github.com/sxyazi/yazi) at commit
// d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: yazi-core/src/input/history.rs.
// Changed for RAMpire: it uses the standard library's map and is recalled
// with the arrows. The text field around it is RAMpire's own. The MIT
// license is in LICENSES/MIT.txt; THIRD_PARTY_NOTICES.md records it.

//! A one-line text field: the usual editing keys, earlier answers to the
//! same question recalled with the arrows, and a suggestion shown after
//! the cursor that Tab or Right takes.

use std::collections::HashMap;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use super::theme::Theme;

/// How many answers each question remembers.
const REMEMBERED: usize = 20;

/// Earlier answers, by question, newest last; for this session only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    inner: HashMap<String, Vec<String>>,
}

impl History {
    pub fn get(&self, group: &str) -> &[String] {
        self.inner.get(group).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Remember `value` for `group`, once, as the newest.
    pub fn remember(&mut self, group: &str, value: &str) -> bool {
        if group.is_empty() || value.is_empty() {
            return false;
        }
        let entries = self.inner.entry(group.to_owned()).or_default();
        if entries.last().is_some_and(|last| last == value) {
            return false;
        }
        entries.retain(|entry| entry != value);
        entries.push(value.to_owned());
        if entries.len() > REMEMBERED {
            entries.drain(..entries.len() - REMEMBERED);
        }
        true
    }
}

/// A text being typed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    value: String,
    /// In characters.
    cursor: usize,
    /// The question it answers, whose earlier answers the arrows recall.
    pub group: Option<String>,
    /// Which earlier answer is shown, counted back from the newest.
    recalled: Option<usize>,
    /// Values offered as a suggestion when they start with what is typed.
    pub offers: Vec<String>,
}

impl Input {
    pub fn new(value: &str) -> Self {
        Self {
            value: value.to_owned(),
            cursor: value.chars().count(),
            ..Self::default()
        }
    }

    /// Recalling earlier answers to `group`.
    pub fn remembering(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    pub fn offering(mut self, offers: Vec<String>) -> Self {
        self.offers = offers;
        self
    }

    pub fn value(&self) -> &str {
        &self.value
    }

    pub fn set(&mut self, value: &str) {
        self.value = value.to_owned();
        self.cursor = value.chars().count();
    }

    /// What Tab would complete it to: the first offer or earlier answer that
    /// starts with what is typed and is longer.
    pub fn suggestion<'a>(&'a self, history: &'a History) -> Option<&'a str> {
        if self.value.is_empty() || self.cursor != self.value.chars().count() {
            return None;
        }
        let typed = self.value.to_lowercase();
        let earlier = self
            .group
            .as_deref()
            .map_or(&[][..], |group| history.get(group));
        earlier
            .iter()
            .rev()
            .chain(&self.offers)
            .find(|offer| {
                offer.len() > self.value.len() && offer.to_lowercase().starts_with(&typed)
            })
            .map(String::as_str)
    }

    fn byte(&self, chars: usize) -> usize {
        self.value
            .char_indices()
            .nth(chars)
            .map_or(self.value.len(), |(index, _)| index)
    }

    fn len(&self) -> usize {
        self.value.chars().count()
    }

    /// The start of the word before the cursor.
    fn word_start(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut at = self.cursor;
        while at > 0 && chars[at - 1].is_whitespace() {
            at -= 1;
        }
        while at > 0 && !chars[at - 1].is_whitespace() {
            at -= 1;
        }
        at
    }

    /// The end of the word after the cursor.
    fn word_end(&self) -> usize {
        let chars: Vec<char> = self.value.chars().collect();
        let mut at = self.cursor;
        while at < chars.len() && chars[at].is_whitespace() {
            at += 1;
        }
        while at < chars.len() && !chars[at].is_whitespace() {
            at += 1;
        }
        at
    }

    fn remove(&mut self, from: usize, to: usize) {
        let (start, end) = (self.byte(from), self.byte(to));
        self.value.replace_range(start..end, "");
        self.cursor = from;
    }

    fn recall(&mut self, history: &History, older: bool) -> bool {
        let Some(group) = &self.group else {
            return false;
        };
        let earlier = history.get(group);
        if earlier.is_empty() {
            return false;
        }
        let next = match (self.recalled, older) {
            (None, true) => Some(0),
            (None, false) => return false,
            (Some(at), true) => Some((at + 1).min(earlier.len() - 1)),
            (Some(0), false) => None,
            (Some(at), false) => Some(at - 1),
        };
        self.recalled = next;
        let value = next
            .and_then(|back| earlier.get(earlier.len() - 1 - back))
            .cloned()
            .unwrap_or_default();
        self.set(&value);
        true
    }

    /// Edit by `key`; whether the field used it. Keys it doesn't use, such
    /// as Enter, Esc and Tab without a suggestion, are the caller's.
    pub fn handle(&mut self, key: &KeyEvent, history: &History) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let end = self.len();
        match key.code {
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = end,
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(end),
            KeyCode::Char('u') if ctrl => self.remove(0, self.cursor),
            KeyCode::Char('k') if ctrl => self.remove(self.cursor, end),
            KeyCode::Char('w') if ctrl => self.remove(self.word_start(), self.cursor),
            KeyCode::Char('h') if ctrl => self.backspace(),
            KeyCode::Char('d') if ctrl => self.delete(),
            KeyCode::Char('b') if alt => self.cursor = self.word_start(),
            KeyCode::Char('f') if alt => self.cursor = self.word_end(),
            KeyCode::Char('d') if alt => self.remove(self.cursor, self.word_end()),
            KeyCode::Char(_) if ctrl || alt => return false,
            KeyCode::Char(character) => {
                let at = self.byte(self.cursor);
                self.value.insert(at, character);
                self.cursor += 1;
                self.recalled = None;
            }
            KeyCode::Backspace if ctrl || alt => self.remove(self.word_start(), self.cursor),
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_start(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_end(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right | KeyCode::Tab if self.cursor == end => {
                let Some(suggestion) = self.suggestion(history).map(str::to_owned) else {
                    return false;
                };
                self.set(&suggestion);
            }
            KeyCode::Right => self.cursor += 1,
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = end,
            KeyCode::Up => return self.recall(history, true),
            KeyCode::Down => return self.recall(history, false),
            _ => return false,
        }
        true
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.remove(self.cursor - 1, self.cursor);
        }
    }

    fn delete(&mut self) {
        if self.cursor < self.len() {
            self.remove(self.cursor, self.cursor + 1);
        }
    }

    /// The field as a line `width` cells wide: the text, the cursor as a
    /// reversed cell, and the suggestion dim after it; scrolled to keep the
    /// cursor in view.
    pub fn line(&self, theme: &Theme, history: &History, width: usize) -> Line<'static> {
        let chars: Vec<char> = self.value.chars().collect();
        let width = width.max(2);
        let skip = (self.cursor + 1).saturating_sub(width);
        let before: String = chars.iter().skip(skip).take(self.cursor - skip).collect();
        let cursor = theme.value().add_modifier(Modifier::REVERSED);
        // At the end, the suggestion continues from under the cursor.
        let suggested: Vec<char> = self
            .suggestion(history)
            .map(|suggestion| suggestion.chars().skip(chars.len()).collect())
            .unwrap_or_default();
        let (under, under_style) = match (chars.get(self.cursor), suggested.first()) {
            (Some(character), _) => (*character, cursor),
            (None, Some(next)) => (*next, theme.dim().add_modifier(Modifier::REVERSED)),
            (None, None) => (' ', cursor),
        };
        let after: String = chars.iter().skip(self.cursor + 1).collect();
        let mut spans = vec![
            Span::styled(before, theme.value()),
            Span::styled(under.to_string(), under_style),
            Span::styled(after, theme.value()),
        ];
        let rest: String = suggested.iter().skip(1).collect();
        if !rest.is_empty() {
            spans.push(Span::styled(rest, theme.dim()));
        }
        Line::from(spans)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    fn typed(input: &mut Input, text: &str) {
        for character in text.chars() {
            input.handle(&key(KeyCode::Char(character)), &History::default());
        }
    }

    #[test]
    fn the_usual_editing_keys_work() {
        let history = History::default();
        let mut input = Input::default();
        typed(&mut input, "max ram");
        input.handle(&ctrl('w'), &history);
        assert_eq!(input.value(), "max ");
        input.handle(&ctrl('a'), &history);
        typed(&mut input, "the ");
        assert_eq!(input.value(), "the max ");
        input.handle(&ctrl('k'), &history);
        assert_eq!(input.value(), "the ");
        input.handle(&key(KeyCode::Left), &history);
        input.handle(&key(KeyCode::Backspace), &history);
        assert_eq!(input.value(), "th ");
        input.handle(&ctrl('u'), &history);
        assert_eq!(input.value(), " ");
        input.handle(&key(KeyCode::Delete), &history);
        assert_eq!(input.value(), "");
        assert!(!input.handle(&key(KeyCode::Enter), &history));
        assert!(!input.handle(&key(KeyCode::Esc), &history));
    }

    #[test]
    fn earlier_answers_come_back_with_the_arrows_and_as_a_suggestion() {
        let mut history = History::default();
        history.remember("reason", "checked by hand");
        history.remember("reason", "rebooted");
        history.remember("reason", "checked by hand");
        assert_eq!(history.get("reason"), ["rebooted", "checked by hand"]);

        let mut input = Input::default().remembering("reason");
        input.handle(&key(KeyCode::Up), &history);
        assert_eq!(input.value(), "checked by hand");
        input.handle(&key(KeyCode::Up), &history);
        assert_eq!(input.value(), "rebooted");
        input.handle(&key(KeyCode::Down), &history);
        input.handle(&key(KeyCode::Down), &history);
        assert_eq!(input.value(), "");

        let mut input = Input::default()
            .remembering("reason")
            .offering(vec!["recovered".to_owned()]);
        typed(&mut input, "re");
        assert_eq!(input.suggestion(&history), Some("rebooted"));
        // Drawn, the suggestion carries on from under the cursor.
        let theme = crate::tui::theme::Theme::new(
            crate::tui::theme::Palette::Midnight,
            crate::tui::theme::Depth::None,
            crate::tui::theme::Icons { unicode: true },
        );
        assert_eq!(input.line(&theme, &history, 20).to_string(), "rebooted");
        typed(&mut input, "c");
        assert_eq!(input.suggestion(&history), Some("recovered"));
        assert!(input.handle(&key(KeyCode::Tab), &history));
        assert_eq!(input.value(), "recovered");
        // Without a suggestion, Tab is the caller's.
        assert!(!input.handle(&key(KeyCode::Tab), &history));
    }

    #[test]
    fn many_answers_keep_the_newest() {
        let mut history = History::default();
        for index in 0..30 {
            history.remember("size", &index.to_string());
        }
        assert_eq!(history.get("size").len(), REMEMBERED);
        assert_eq!(history.get("size")[0], "10");
        assert!(!history.remember("", "x"));
    }
}
