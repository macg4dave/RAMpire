// Copyright (c) 2023 - sxyazi
// Copyright (c) 2026 macg4dave
// SPDX-License-Identifier: MIT
//
// Adapted from Yazi (https://github.com/sxyazi/yazi) at commit
// d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: yazi-core/src/which/which.rs
// (narrowing the candidates as keys are typed) and
// yazi-fm/src/which/{which,cand}.rs (the popup: bordered, at the bottom, in
// up to three columns of key and description). Changed for RAMpire: a
// candidate runs a RAMpire command or a VM menu's item; the popup also
// serves as a VM's menu, opened beside its row, with a cursor, a few lines
// about the VM and Shift with a letter to copy the item's command line; it
// is drawn in RAMpire's theme. The MIT license is in LICENSES/MIT.txt;
// THIRD_PARTY_NOTICES.md records it.

//! The popup of what can follow: a chord's next keys (`g` then `g`, `d`,
//! `l` ...), or a VM's menu, each item with its key.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::KeyCode;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph, Widget};

use super::app::Run;
use super::keymap::{Binding, Key};
use super::theme::{Icon, Theme};
use super::widgets::{self, raised_panel};
use crate::tone::Tone;

const PADDING_X: u16 = 1;
const PADDING_Y: u16 = 1;

/// One thing that can follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cand {
    /// The keys still to type, the first next.
    pub keys: Vec<Key>,
    pub desc: String,
    /// The command line that does the same, for a VM menu's item.
    pub command: Option<String>,
    pub run: Run,
}

/// Where the popup sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Along the bottom, as Yazi's.
    Bottom,
    /// Beside the chosen VM's row.
    Hovered,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Which {
    pub title: String,
    /// Lines about what the menu is for, above its items.
    pub text: Vec<String>,
    pub cands: Vec<Cand>,
    /// Keys typed so far.
    pub times: usize,
    pub anchor: Anchor,
    /// A menu's chosen item, for Enter.
    pub cursor: usize,
    /// A VM's menu, rather than a chord.
    pub menu: bool,
}

/// What a key did to the popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Still open, narrowed or moved.
    Open,
    /// Closed, running this, or nothing.
    Done(Option<Run>),
}

impl Which {
    /// The popup after a chord's first key, with the bindings it starts.
    pub fn chord(first: Key, bindings: &[&'static Binding]) -> Self {
        Self {
            title: first.label(),
            text: Vec::new(),
            cands: bindings
                .iter()
                .map(|binding| Cand {
                    keys: binding.on.to_vec(),
                    desc: binding.desc.to_owned(),
                    command: None,
                    run: Run::Cmd(binding.cmd),
                })
                .collect(),
            times: 1,
            anchor: Anchor::Bottom,
            cursor: 0,
            menu: false,
        }
    }

    /// A VM's menu.
    pub fn menu(title: String, text: Vec<String>, cands: Vec<Cand>) -> Self {
        Self {
            title,
            text,
            cands,
            times: 0,
            anchor: Anchor::Hovered,
            cursor: 0,
            menu: true,
        }
    }

    /// `key` typed in the popup: narrow the candidates to those it starts,
    /// and run one that is complete.
    pub fn r#type(&mut self, key: Key) -> Typed {
        if self.menu {
            return self.type_menu(key);
        }
        if key.code == KeyCode::Esc {
            return Typed::Done(None);
        }
        let times = self.times;
        self.cands
            .retain(|cand| cand.keys.len() > times && cand.keys[times] == key);
        self.times += 1;
        if self.cands.is_empty() {
            Typed::Done(None)
        } else if self.cands.len() == 1 {
            Typed::Done(Some(self.cands.remove(0).run))
        } else if let Some(at) = self
            .cands
            .iter()
            .position(|cand| cand.keys.len() == self.times)
        {
            Typed::Done(Some(self.cands.remove(at).run))
        } else {
            Typed::Open
        }
    }

    fn type_menu(&mut self, key: Key) -> Typed {
        let count = self.cands.len();
        match key.code {
            KeyCode::Esc => Typed::Done(None),
            KeyCode::Up => {
                self.cursor = (self.cursor + count.max(1) - 1) % count.max(1);
                Typed::Open
            }
            KeyCode::Down => {
                self.cursor = (self.cursor + 1) % count.max(1);
                Typed::Open
            }
            KeyCode::Enter => Typed::Done(self.cands.get(self.cursor).map(|cand| cand.run.clone())),
            KeyCode::Char(character) if key.modifiers.is_empty() => {
                // A letter does its item; the letter with Shift copies its
                // command line.
                let lower = character.to_ascii_lowercase();
                let found = self
                    .cands
                    .iter()
                    .find(|cand| cand.keys.first() == Some(&Key::ch(lower)));
                match found {
                    Some(cand) if character.is_ascii_uppercase() => {
                        Typed::Done(cand.command.clone().map(Run::Copy))
                    }
                    Some(cand) => Typed::Done(Some(cand.run.clone())),
                    None => Typed::Open,
                }
            }
            _ => Typed::Open,
        }
    }

    /// The keys still to type for `cand`, as the help writes them.
    fn keys_left(&self, cand: &Cand) -> String {
        cand.keys
            .iter()
            .skip(self.times)
            .map(Key::label)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Draw it in `area`; a menu goes under `row`, or over it near the
    /// bottom.
    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, row: Option<u16>) {
        if self.menu {
            self.render_menu(area, buf, theme, row);
        } else {
            self.render_chord(area, buf, theme);
        }
    }

    fn render_chord(&self, area: Rect, buf: &mut Buffer, theme: &Theme) {
        let cols: usize = match area.width {
            0..80 => 1,
            80..120 => 2,
            _ => 3,
        };
        let height = area
            .height
            .min(self.cands.len().div_ceil(cols) as u16 + PADDING_Y * 2);
        let rect = Rect {
            x: area.x + PADDING_X.min(area.width),
            y: area.bottom().saturating_sub(height + PADDING_Y),
            width: area.width.saturating_sub(PADDING_X * 2),
            height,
        };
        // Don't draw if there's no room.
        if rect.height <= PADDING_Y * 2 {
            return;
        }
        Clear.render(rect, buf);
        let block = raised_panel(theme, Icon::Pointer, &self.title, "then");
        let inner = block.inner(rect);
        block.render(rect, buf);
        let chunks = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(inner);
        for y in 0..inner.height {
            for (x, chunk) in chunks.iter().enumerate() {
                let Some(cand) = self.cands.get(y as usize * cols + x) else {
                    break;
                };
                let line = Line::from(vec![
                    Span::styled(format!(" {} ", self.keys_left(cand)), theme.key()),
                    Span::styled(format!(" {}", cand.desc), theme.text()),
                ]);
                let cell = Rect {
                    y: chunk.y + y,
                    height: 1,
                    ..*chunk
                };
                Paragraph::new(line).style(theme.raised()).render(cell, buf);
            }
        }
    }

    fn render_menu(&self, area: Rect, buf: &mut Buffer, theme: &Theme, row: Option<u16>) {
        let width = area.width.saturating_sub(4).min(72);
        let inner_width = width.saturating_sub(4) as usize;
        let mut lines: Vec<Line<'static>> = self
            .text
            .iter()
            .flat_map(|text| {
                widgets::wrap(
                    widgets::line(theme, text.clone(), Tone::Normal),
                    inner_width,
                )
            })
            .collect();
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        for (index, cand) in self.cands.iter().enumerate() {
            let style = if index == self.cursor {
                theme.selected()
            } else {
                theme.value()
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", self.keys_left(cand)), theme.key()),
                Span::styled(format!(" {} ", cand.desc), style),
            ]));
        }
        lines.push(Line::default());
        if let Some(command) = self
            .cands
            .get(self.cursor)
            .and_then(|cand| cand.command.as_deref())
        {
            lines.extend(widgets::wrap(
                widgets::line(theme, format!("same as: {command}"), Tone::Dim),
                inner_width,
            ));
        }
        lines.push(widgets::line(
            theme,
            "letter or Enter: do it   Shift+letter: copy its command   Esc: back",
            Tone::Dim,
        ));
        let height = (lines.len() as u16 + 2).min(area.height);
        // Under the row when it fits, else above it, else centred.
        let x = area.x + area.width.saturating_sub(width) / 2;
        let y = match row {
            Some(row) if row + 1 + height <= area.bottom() => row + 1,
            Some(row) if row >= area.y + height => row - height,
            _ => area.y + area.height.saturating_sub(height) / 2,
        };
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        Clear.render(rect, buf);
        let block = raised_panel(theme, Icon::Vm, &self.title, "");
        let inner = block.inner(rect);
        block.render(rect, buf);
        Paragraph::new(lines)
            .style(theme.raised())
            .render(inner, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::keymap::{lookup, Cmd, Layer, Lookup};
    use crate::tui::scroll::Step;

    #[test]
    fn a_chord_narrows_as_keys_are_typed_and_runs_when_complete() {
        let Lookup::Chord(chords) = lookup(Layer::List, Key::ch('g')) else {
            panic!("g starts chords");
        };
        let mut which = Which::chord(Key::ch('g'), &chords);
        assert!(which.cands.len() > 2);
        assert_eq!(
            which.r#type(Key::ch('g')),
            Typed::Done(Some(Run::Cmd(Cmd::Move(Step::Top))))
        );
        let mut which = Which::chord(Key::ch('g'), &chords);
        assert_eq!(which.r#type(Key::ch('z')), Typed::Done(None));
    }

    #[test]
    fn a_menus_letter_runs_its_item_and_shift_copies_its_command() {
        let cand = |key: char, command: &str| Cand {
            keys: vec![Key::ch(key)],
            desc: "Details".to_owned(),
            command: Some(command.to_owned()),
            run: Run::Cmd(Cmd::Spot),
        };
        let mut which = Which::menu(
            "web".to_owned(),
            vec![],
            vec![
                cand('v', "rampire watch web"),
                cand('o', "rampire doctor web"),
            ],
        );
        assert_eq!(which.r#type(Key::of(KeyCode::Down)), Typed::Open);
        assert_eq!(which.cursor, 1);
        assert_eq!(
            which.r#type(Key::ch('O')),
            Typed::Done(Some(Run::Copy("rampire doctor web".to_owned())))
        );
        assert_eq!(
            which.r#type(Key::ch('v')),
            Typed::Done(Some(Run::Cmd(Cmd::Spot)))
        );
        assert_eq!(which.r#type(Key::ch('x')), Typed::Open);
    }
}
