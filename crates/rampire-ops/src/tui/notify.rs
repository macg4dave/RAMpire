// Copyright (c) 2023 - sxyazi
// Copyright (c) 2026 macg4dave
// SPDX-License-Identifier: MIT
//
// Adapted from Yazi (https://github.com/sxyazi/yazi) at commit
// d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: yazi-fm/src/notify/notify.rs,
// yazi-core/src/notify/{notify,message,mod}.rs and
// yazi-actor/src/notify/tick.rs. Changed for RAMpire: the messages take
// their level from a `Tone` and their colours from the TUI's theme, they
// slide in and out by the time since they were shown rather than by a
// ticker task, and they are drawn with RAMpire's panel borders. The MIT
// license is in LICENSES/MIT.txt; THIRD_PARTY_NOTICES.md records it.

//! Notices that slide in at the bottom right, each a titled box as wide as
//! its text, stacked newest at the bottom, as many as fit; each slides out
//! again after a time that depends on how much it matters.

use std::time::{Duration, Instant};

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Offset, Rect};
use ratatui::text::Span;
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use super::theme::{Icon, Theme};
use crate::tone::Tone;

/// A box's border, left and right or top and bottom.
const BORDER: u16 = 2;
/// Rows between boxes.
const SPACING: u16 = 1;
/// How long sliding in or out takes.
const SLIDE: Duration = Duration::from_millis(250);
/// The most boxes shown at once.
const MOST: usize = 3;

/// One notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub title: String,
    pub content: String,
    pub tone: Tone,
    /// How long it stays, once in.
    pub timeout: Duration,
    pub shown: Instant,
}

impl Message {
    /// A notice of `content`, titled and timed by how much it matters.
    pub fn new(content: impl Into<String>, tone: Tone) -> Self {
        let (title, timeout) = match tone {
            Tone::Bad => ("Error", Duration::from_secs(10)),
            Tone::Warn => ("Note", Duration::from_secs(6)),
            Tone::Good => ("Done", Duration::from_secs(4)),
            _ => ("RAMpire", Duration::from_secs(4)),
        };
        Self {
            title: title.to_owned(),
            content: content.into(),
            tone,
            timeout,
            shown: Instant::now(),
        }
    }

    /// How far in it has slid, 0 to 100, at `now`: in over [`SLIDE`], out
    /// over the same once its time is up.
    pub fn percent(&self, now: Instant) -> u8 {
        let age = now.saturating_duration_since(self.shown);
        let slide = SLIDE.as_millis().max(1);
        let leaving = age.saturating_sub(SLIDE + self.timeout);
        let percent = if leaving > Duration::ZERO {
            100u128.saturating_sub(leaving.as_millis() * 100 / slide)
        } else {
            (age.as_millis() * 100 / slide).min(100)
        };
        percent as u8
    }

    /// Whether it has slid out and can go.
    pub fn gone(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.shown) >= SLIDE * 2 + self.timeout
    }

    fn icon(&self) -> Icon {
        match self.tone {
            Tone::Bad | Tone::Warn => Icon::Problem,
            Tone::Good => Icon::Ok,
            _ => Icon::Running,
        }
    }

    /// Its box's width: the widest of its lines and its title.
    pub fn width(&self, theme: &Theme) -> u16 {
        let title = Span::raw(format!("{} {} ", theme.icon(self.icon()), self.title)).width();
        let content = self
            .content
            .lines()
            .map(|line| Span::raw(line).width())
            .max()
            .unwrap_or(0);
        (content.max(title) as u16).saturating_add(BORDER + 2)
    }

    /// Its box's height at `width`, its text wrapped.
    pub fn height(&self, width: u16) -> u16 {
        let lines = Paragraph::new(self.content.as_str())
            .wrap(Wrap { trim: false })
            .line_count(width.saturating_sub(BORDER + 2));
        lines as u16 + BORDER
    }
}

/// The notices showing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Notify {
    /// Newest last.
    pub messages: Vec<Message>,
}

impl Notify {
    pub fn push(&mut self, message: Message) {
        // The same notice again starts over rather than stacking.
        self.messages.retain(|old| {
            old.content != message.content || old.tone != message.tone || old.title != message.title
        });
        self.messages.push(message);
    }

    /// Drop the notices that have slid out.
    pub fn tick(&mut self, now: Instant) {
        self.messages.retain(|message| !message.gone(now));
    }

    /// Whether any notice is showing, so the screen keeps redrawing.
    pub fn busy(&self) -> bool {
        !self.messages.is_empty()
    }

    /// Where notices go: the right of the screen, at most 80 columns,
    /// above the last line.
    pub fn available(area: Rect) -> Rect {
        let [_, right] = Layout::horizontal([Constraint::Fill(1), Constraint::Max(80)]).areas(area);
        let [above, _] = Layout::vertical([Constraint::Fill(1), Constraint::Max(1)]).areas(right);
        above
    }

    /// How many of the newest notices fit in `area`, at least one.
    pub fn limit(&self, area: Rect, theme: &Theme) -> usize {
        let mut height = area.height;
        let mut fit = 0;
        for message in self.messages.iter().rev().take(MOST) {
            let width = message.width(theme).min(area.width);
            match height.checked_sub(message.height(width) + SPACING) {
                Some(left) => {
                    height = left;
                    fit += 1;
                }
                None => break,
            }
        }
        fit.max(usize::from(!self.messages.is_empty()))
    }

    /// Each notice's box: stacked at the bottom of `area`, newest lowest,
    /// each at the right and as wide as it needs.
    fn tiles(area: Rect, messages: &[&Message], theme: &Theme) -> Vec<Rect> {
        let sizes: Vec<(u16, u16)> = messages
            .iter()
            .map(|message| {
                let width = message.width(theme).min(area.width);
                (width, message.height(width))
            })
            .collect();
        Layout::vertical(
            std::iter::once(Constraint::Fill(1))
                .chain(sizes.iter().map(|(_, height)| Constraint::Length(*height))),
        )
        .spacing(SPACING)
        .split(area)
        .iter()
        .skip(1)
        .zip(&sizes)
        .map(|(&tile, (width, _))| {
            let mut tile = tile;
            if let Some(spare) = tile.width.checked_sub(*width) {
                (tile.x, tile.width) = (tile.x.saturating_add(spare), *width);
            }
            tile
        })
        .collect()
    }

    pub fn render(&self, area: Rect, buf: &mut Buffer, theme: &Theme, now: Instant) {
        let available = Self::available(area);
        let limit = self.limit(available, theme);
        let messages: Vec<&Message> = self.messages.iter().rev().take(limit).rev().collect();
        let tiles = Self::tiles(available, &messages, theme);
        for (message, tile) in messages.iter().zip(tiles) {
            // Slide in from the right edge, and out to it.
            let hidden = u32::from(100 - message.percent(now)) * u32::from(tile.width) / 100;
            let mut rect = tile.offset(Offset {
                x: hidden as i32,
                y: 0,
            });
            rect = rect.intersection(available);
            if rect.width == 0 {
                continue;
            }
            Clear.render(rect, buf);
            let style = theme.tone(message.tone);
            Paragraph::new(message.content.as_str())
                .wrap(Wrap { trim: false })
                .style(theme.raised().patch(theme.value()))
                .block(
                    Block::new()
                        .borders(Borders::ALL)
                        .border_set(theme.border_set())
                        .border_style(style)
                        .style(theme.raised())
                        .padding(ratatui::widgets::Padding::horizontal(1))
                        .title(Span::styled(
                            format!(" {} {} ", theme.icon(message.icon()), message.title),
                            style,
                        )),
                )
                .render(rect, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Depth, Icons, Palette};

    fn theme() -> Theme {
        Theme::new(Palette::Terminal, Depth::Colours16, Icons { unicode: true })
    }

    #[test]
    fn a_notice_slides_in_stays_and_slides_out() {
        let message = Message::new("ran: rampire apply", Tone::Good);
        let at = message.shown;
        assert_eq!(message.percent(at), 0);
        assert_eq!(message.percent(at + SLIDE / 2), 50);
        assert_eq!(message.percent(at + SLIDE + Duration::from_secs(1)), 100);
        let leaving = at + SLIDE + message.timeout + SLIDE / 2;
        assert_eq!(message.percent(leaving), 50);
        assert!(!message.gone(leaving));
        assert!(message.gone(at + SLIDE * 2 + message.timeout));
        // An error stays longer than a done.
        assert!(Message::new("x", Tone::Bad).timeout > message.timeout);
    }

    #[test]
    fn notices_stack_at_the_bottom_right_as_wide_as_their_text() {
        let theme = theme();
        let mut notify = Notify::default();
        notify.push(Message::new("short", Tone::Good));
        notify.push(Message::new(
            "a much longer notice about what happened",
            Tone::Bad,
        ));
        notify.push(Message::new("short", Tone::Good));
        // The repeated notice moved to the end rather than stacking twice.
        assert_eq!(notify.messages.len(), 2);
        assert_eq!(notify.messages[1].content, "short");

        let area = Rect::new(0, 0, 120, 30);
        let mut buf = Buffer::empty(area);
        let later = Instant::now() + SLIDE * 2;
        notify.render(area, &mut buf, &theme, later);
        let text = crate::tui::widgets::plain_text(&buf);
        let lines: Vec<&str> = text.lines().collect();
        let long = lines
            .iter()
            .position(|line| line.contains("a much longer"))
            .unwrap();
        let short = lines
            .iter()
            .position(|line| line.contains("short"))
            .unwrap();
        assert!(long < short, "newest lowest:\n{text}");
        assert!(
            lines[short].trim_end().ends_with('│'),
            "at the right:\n{text}"
        );
        assert!(text.contains("Error") && text.contains("Done"), "{text}");
        // Nothing on the last line, where the status bar is.
        assert!(lines.len() < 30 || lines[29].trim().is_empty());
    }

    #[test]
    fn as_many_fit_as_the_height_allows_and_always_one() {
        let theme = theme();
        let mut notify = Notify::default();
        for index in 0..5 {
            notify.push(Message::new(format!("notice {index}"), Tone::Good));
        }
        assert_eq!(notify.limit(Rect::new(0, 0, 80, 40), &theme), MOST);
        assert_eq!(notify.limit(Rect::new(0, 0, 80, 4), &theme), 1);
        assert_eq!(notify.limit(Rect::new(0, 0, 80, 1), &theme), 1);
    }
}
