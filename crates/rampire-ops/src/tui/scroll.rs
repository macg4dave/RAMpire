// Copyright (c) 2023 - sxyazi
// Copyright (c) 2026 macg4dave
// SPDX-License-Identifier: MIT
//
// Adapted from Yazi (https://github.com/sxyazi/yazi) at commit
// d5e4beb6b5212c28d1026d8e7d4ae985dcf174cb: yazi-widgets/src/scrollable.rs
// and yazi-widgets/src/step.rs. Changed for RAMpire: the trait became one
// `Scroll` value that each list keeps, which learns its height when it is
// drawn; the step's viewport-relative form and its parsing were left out;
// the scroll-off margin is a fixed few rows. The MIT license is in
// LICENSES/MIT.txt; THIRD_PARTY_NOTICES.md records it.

//! One way to move through every list: a cursor with a margin kept above
//! and below it, moved a row, half a page, a page, or to either end.

use std::cell::Cell;
use std::ops::Range;

/// The rows kept in view above and below the cursor, at most half the
/// view.
const SCROLLOFF: usize = 3;

/// A move of the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Top,
    Bottom,
    /// One up, from the top to the bottom.
    Prev,
    /// One down, from the bottom to the top.
    Next,
    Offset(isize),
    /// A share of the view: 50 is half a page down, -100 a page up.
    Percent(i8),
}

impl Step {
    /// The cursor's place after this step from `pos`, in a list of `len`
    /// rows shown `limit` at a time.
    pub fn add(self, pos: usize, len: usize, limit: usize) -> usize {
        if len == 0 {
            return 0;
        }
        let off = match self {
            Self::Top => return 0,
            Self::Bottom => return len - 1,
            Self::Prev => -1,
            Self::Next => 1,
            Self::Offset(n) => n,
            Self::Percent(0) => 0,
            Self::Percent(n) => n as isize * limit.max(1) as isize / 100,
        };
        if matches!(self, Self::Prev | Self::Next) {
            off.saturating_add_unsigned(pos).rem_euclid(len as isize) as usize
        } else {
            pos.saturating_add_signed(off)
        }
        .min(len - 1)
    }
}

/// A list's cursor and the first row shown. The view's height is learnt
/// when the list is drawn, so a page is a page of what is on the screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scroll {
    pub cursor: usize,
    offset: Cell<usize>,
    limit: Cell<usize>,
}

impl Scroll {
    /// At `cursor`.
    pub fn at(cursor: usize) -> Self {
        Self {
            cursor,
            ..Self::default()
        }
    }

    fn scrolloff(&self) -> usize {
        SCROLLOFF.min(self.limit.get() / 2)
    }

    /// The cursor within a list of `total` rows.
    pub fn cursor(&self, total: usize) -> usize {
        self.cursor.min(total.saturating_sub(1))
    }

    /// Move by `step` in a list of `total` rows; whether anything moved.
    pub fn step(&mut self, step: Step, total: usize) -> bool {
        let old = self.cursor(total);
        let new = step.add(old, total, self.limit.get());
        if new > old {
            self.next(new, total)
        } else {
            self.prev(new, total)
        }
    }

    fn next(&mut self, new: usize, total: usize) -> bool {
        let (old_cursor, old_offset) = (self.cursor, self.offset.get());
        let limit = self.limit.get();
        let visible_end = total.min(old_offset + limit);
        let offset = if new >= visible_end.saturating_sub(self.scrolloff()) {
            total
                .saturating_sub(limit)
                .min(old_offset + new.saturating_sub(old_cursor))
        } else {
            old_offset.min(total.saturating_sub(1))
        };
        self.cursor = new;
        self.offset.set(offset);
        (new, offset) != (old_cursor, old_offset)
    }

    fn prev(&mut self, new: usize, total: usize) -> bool {
        let (old_cursor, old_offset) = (self.cursor, self.offset.get());
        let offset = if new < old_offset + self.scrolloff() {
            old_offset.saturating_sub(old_cursor.saturating_sub(new))
        } else {
            total.saturating_sub(1).min(old_offset)
        };
        self.cursor = new;
        self.offset.set(offset);
        (new, offset) != (old_cursor, old_offset)
    }

    /// The rows to draw of `total`, `limit` of them in view: the cursor in
    /// view with its margin, the list never scrolled past its end. Drawing
    /// calls this, so the next page step knows the view's height.
    pub fn window(&self, total: usize, limit: usize) -> Range<usize> {
        self.limit.set(limit);
        if total == 0 || limit == 0 {
            self.offset.set(0);
            return 0..0;
        }
        let cursor = self.cursor(total);
        let margin = self.scrolloff();
        let mut offset = self.offset.get();
        if cursor < offset + margin {
            offset = cursor.saturating_sub(margin);
        }
        if cursor + margin >= offset + limit {
            offset = (cursor + margin + 1).saturating_sub(limit);
        }
        offset = offset.min(total.saturating_sub(limit));
        self.offset.set(offset);
        offset..(offset + limit).min(total)
    }

    /// The first row in view, as last drawn.
    pub fn offset(&self) -> usize {
        self.offset.get()
    }
}

/// Text read from its newest line back, as a task's output: it follows the
/// newest line until scrolled back, and the same steps move it. (RAMpire's
/// own, beside Yazi's list scrolling.)
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pager {
    /// Rows scrolled back from the newest.
    back: usize,
    /// The rows and the view's height, as last drawn.
    total: Cell<usize>,
    height: Cell<usize>,
}

impl Pager {
    /// Scroll by `step` through the rows last drawn.
    pub fn step(&mut self, step: Step) {
        let most = self.total.get().saturating_sub(self.height.get());
        let page = self.height.get().max(1) as isize;
        self.back = match step {
            Step::Top => most,
            Step::Bottom => 0,
            Step::Prev => self.back + 1,
            Step::Next => self.back.saturating_sub(1),
            Step::Offset(n) => self.back.saturating_add_signed(-n),
            Step::Percent(n) => self.back.saturating_add_signed(-(n as isize) * page / 100),
        }
        .min(most);
    }

    /// The rows in view of `total`, `height` of them, newest at the bottom.
    pub fn window(&self, total: usize, height: usize) -> Range<usize> {
        self.total.set(total);
        self.height.set(height);
        let end = total.saturating_sub(self.back.min(total.saturating_sub(height)));
        end.saturating_sub(height)..end
    }

    /// Back to following the newest line.
    pub fn follow(&mut self) {
        self.back = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_move_by_rows_pages_and_to_either_end() {
        assert_eq!(Step::Next.add(9, 10, 5), 0, "down from the bottom wraps");
        assert_eq!(Step::Prev.add(0, 10, 5), 9, "up from the top wraps");
        assert_eq!(Step::Percent(50).add(0, 100, 20), 10);
        assert_eq!(Step::Percent(-100).add(5, 100, 20), 0);
        assert_eq!(Step::Offset(500).add(5, 10, 5), 9);
        assert_eq!(Step::Bottom.add(0, 10, 5), 9);
        assert_eq!(Step::Top.add(7, 10, 5), 0);
        assert_eq!(Step::Next.add(0, 0, 5), 0);
    }

    #[test]
    fn the_cursor_stays_in_view_with_a_margin() {
        let mut scroll = Scroll::default();
        assert_eq!(scroll.window(100, 10), 0..10);
        for _ in 0..7 {
            scroll.step(Step::Next, 100);
        }
        // The cursor at 7 keeps three rows beneath it: the view moved one.
        assert_eq!(scroll.window(100, 10), 1..11);
        scroll.step(Step::Bottom, 100);
        assert_eq!(scroll.window(100, 10), 90..100);
        scroll.step(Step::Percent(-50), 100);
        assert_eq!(scroll.cursor, 94);
        // Half a page up from the bottom leaves the view where it was.
        assert_eq!(scroll.window(100, 10), 90..100);
        // A shorter list brings the cursor and the view back within it.
        assert_eq!(scroll.window(4, 10), 0..4);
        assert_eq!(scroll.cursor(4), 3);
        // Back to the long list, the cursor is in view with its margin.
        assert_eq!(scroll.window(100, 10), 88..98);
        scroll.step(Step::Top, 100);
        assert_eq!(scroll.window(100, 10), 0..10);
        assert_eq!(scroll.window(0, 10), 0..0);
    }

    #[test]
    fn a_pager_follows_the_newest_line_until_scrolled_back() {
        let mut pager = Pager::default();
        assert_eq!(pager.window(50, 10), 40..50);
        pager.step(Step::Prev);
        assert_eq!(pager.window(50, 10), 39..49);
        pager.step(Step::Top);
        assert_eq!(pager.window(50, 10), 0..10);
        pager.step(Step::Percent(50));
        assert_eq!(pager.window(50, 10), 5..15);
        pager.step(Step::Bottom);
        assert_eq!(pager.window(50, 10), 40..50);
        assert_eq!(pager.window(3, 10), 0..3);
    }
}
