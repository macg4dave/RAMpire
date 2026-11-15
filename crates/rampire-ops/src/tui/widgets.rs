//! The drawing pieces every screen uses: panels, bars, figure lines, the
//! pool's stacked bar and history, and a screen as plain text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use super::theme::{Icon, Theme};
use crate::tone::{self, Tone};

/// A panel: a card with rounded corners, titled with a chip that holds its
/// icon and name, and a dim note after it.
pub fn panel<'a>(theme: &Theme, icon: Icon, title: &str, detail: &str) -> Block<'a> {
    let mut spans = vec![
        Span::styled(" ", theme.frame_of(icon)),
        Span::styled(
            format!(" {} {} ", theme.icon(icon), title),
            theme.chip_of(icon),
        ),
        Span::styled(" ", theme.frame_of(icon)),
    ];
    if !detail.is_empty() {
        spans.push(Span::styled(format!("{detail} "), theme.dim()));
    }
    Block::new()
        .borders(Borders::ALL)
        .border_set(theme.border_set())
        .border_style(theme.frame_of(icon))
        .style(theme.card())
        .padding(ratatui::widgets::Padding::horizontal(1))
        .title(Line::from(spans))
}

/// A dialog's panel: raised above the cards, with an accent border.
pub fn raised_panel<'a>(theme: &Theme, icon: Icon, title: &str, detail: &str) -> Block<'a> {
    panel(theme, icon, title, detail)
        .style(theme.raised())
        .border_style(theme.accent())
}

/// A bar `width` cells wide, `fraction` of it full.
pub fn bar<'a>(theme: &Theme, fraction: f64, width: usize, tone: Tone) -> Vec<Span<'a>> {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = ((fraction * width as f64).round() as usize).min(width);
    vec![
        Span::styled(theme.icon(Icon::Line).repeat(filled), theme.tone(tone)),
        Span::styled(theme.icon(Icon::Rule).repeat(width - filled), theme.track()),
    ]
}

/// `label  [bar]  42.0%  detail`, the bar's tone from how full it is
/// unless one is given.
pub fn meter<'a>(
    theme: &Theme,
    label: &str,
    fraction: f64,
    width: usize,
    detail: String,
    tone: Option<Tone>,
) -> Line<'a> {
    let tone = tone.unwrap_or_else(|| tone::load(fraction));
    let mut spans = vec![Span::styled(format!("{label:<12}"), theme.text())];
    spans.extend(bar(theme, fraction, width, tone));
    spans.push(Span::styled(
        format!(" {:5.1}%", fraction.clamp(0.0, 1.0) * 100.0),
        theme.tone(tone),
    ));
    spans.push(Span::styled(format!("  {detail}"), theme.text()));
    Line::from(spans)
}

/// `label        value    detail`.
pub fn metric<'a>(
    theme: &Theme,
    label: &str,
    value: String,
    detail: String,
    tone: Tone,
) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<18}"), theme.text()),
        Span::styled(format!("{value:>12}"), theme.tone(tone)),
        Span::styled(format!("   {detail}"), theme.dim()),
    ])
}

/// The tone's icon before a word, so a meaning never rests on colour.
pub fn marked<'a>(theme: &Theme, icon: Icon, text: String, tone: Tone) -> Span<'a> {
    Span::styled(format!("{} {text}", theme.icon(icon)), theme.tone(tone))
}

/// The pool's charge split by VM: each VM's share in its series colour,
/// then the free part.
pub fn stacked_bar<'a>(theme: &Theme, charges: &[u64], scale: u64, cells: usize) -> Vec<Span<'a>> {
    let mut spans = Vec::new();
    let mut start = 0;
    for (index, end) in crate::top::segment_ends(charges, scale, cells)
        .into_iter()
        .enumerate()
    {
        if end > start {
            spans.push(Span::styled(
                theme.icon(Icon::Full).repeat(end - start),
                theme.series(index),
            ));
        }
        start = end;
    }
    spans.push(Span::styled(
        theme.icon(Icon::Empty).repeat(cells.saturating_sub(start)),
        theme.track(),
    ));
    spans
}

/// The pool's history, stacked by VM, newest on the right, with a scale on
/// the left.
pub struct StackedHistory<'a> {
    pub theme: &'a Theme,
    /// Each sample's charge per VM, oldest first.
    pub history: &'a [Vec<u64>],
    pub scale: u64,
}

impl Widget for StackedHistory<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        const AXIS: u16 = 6;
        if area.width <= AXIS || area.height == 0 {
            return;
        }
        let columns = (area.width - AXIS) as usize;
        let points: Vec<&Vec<u64>> = self.history.iter().rev().take(columns).rev().collect();
        let height = area.height as usize;
        let scale = self.scale.max(1);
        let gib = scale as f64 / crate::dashboard::GIB as f64;
        let axis = if self.theme.icons.unicode { "┤" } else { "|" };
        for row in 0..height {
            let y = area.y + row as u16;
            let level = height - row;
            let label = if row == 0 || row + 1 == height {
                format!("{:5.1}", gib * level as f64 / height as f64)
            } else {
                String::new()
            };
            buf.set_string(area.x, y, format!("{label:>5}{axis}"), self.theme.dim());
            // Half-cell resolution: a cell belongs to the VM covering its middle.
            let middle = level * 2 - 1;
            for (column, charges) in points.iter().enumerate() {
                let ends = crate::top::segment_ends(charges, scale, height * 2);
                let x = area.x + AXIS + column as u16;
                match ends.iter().position(|end| *end > middle) {
                    Some(index) => {
                        buf.set_string(x, y, self.theme.icon(Icon::Full), self.theme.series(index));
                    }
                    None => {
                        buf.set_string(x, y, " ", self.theme.base());
                    }
                }
            }
        }
    }
}

/// A screen as plain text: each row with its trailing spaces removed, and
/// trailing empty rows dropped.
pub fn plain_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    let mut lines = Vec::new();
    for y in area.top()..area.bottom() {
        let mut line = String::new();
        let mut skip = 0;
        for x in area.left()..area.right() {
            if skip > 0 {
                skip -= 1;
                continue;
            }
            let cell = &buffer[(x, y)];
            let symbol = cell.symbol();
            line.push_str(symbol);
            skip = unicode_width(symbol).saturating_sub(1);
        }
        lines.push(line.trim_end().to_owned());
    }
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut text = lines.join("\n");
    text.push('\n');
    text
}

fn unicode_width(symbol: &str) -> usize {
    use ratatui::text::Span;
    Span::raw(symbol).width()
}

/// Text cut to `width` characters, with an ellipsis when cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    cut.push('~');
    cut
}

/// A style for a whole line of `text` in a tone.
pub fn line<'a>(theme: &Theme, text: impl Into<String>, tone: Tone) -> Line<'a> {
    Line::from(Span::styled(text.into(), theme.tone(tone)))
}

/// An empty style; for spans that take the area's own.
pub fn none() -> Style {
    Style::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::theme::{Depth, Icons, Palette};

    #[test]
    fn plain_text_drops_trailing_space_and_rows() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 10, 4));
        buffer.set_string(0, 0, "a b", Style::new());
        buffer.set_string(2, 1, "c", Style::new());
        assert_eq!(plain_text(&buffer), "a b\n  c\n");
    }

    #[test]
    fn bars_are_as_wide_as_asked_in_either_icon_set() {
        for unicode in [true, false] {
            let theme = Theme::new(Palette::Midnight, Depth::None, Icons { unicode });
            let spans = bar(&theme, 0.25, 8, Tone::Good);
            let cells: usize = spans.iter().map(|span| span.content.chars().count()).sum();
            assert_eq!(cells, 8);
            let spans = stacked_bar(&theme, &[1, 1], 4, 10);
            let cells: usize = spans.iter().map(|span| span.content.chars().count()).sum();
            assert_eq!(cells, 10);
        }
        assert_eq!(fit("abcdef", 4), "abc~");
        assert_eq!(fit("abc", 4), "abc");
    }
}
