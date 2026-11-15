//! The drawing pieces every screen uses: panels, bars, figure lines, the
//! pool's stacked bar and history, and a screen as plain text.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};

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

/// `line` cut into rows of at most `width` columns at its spaces, keeping
/// each span's style; a word wider than a row is cut where the row ends.
pub fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    // Words with the spaces after them, each with its span's style.
    let mut words: Vec<(String, Style)> = Vec::new();
    for span in line.spans {
        let mut word = String::new();
        for character in span.content.chars() {
            if character != ' ' && word.ends_with(' ') {
                words.push((std::mem::take(&mut word), span.style));
            }
            word.push(character);
        }
        if !word.is_empty() {
            words.push((word, span.style));
        }
    }
    let cells = |text: &str| Span::raw(text.to_owned()).width();
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for (word, style) in words {
        let trimmed = cells(word.trim_end());
        if used > 0 && used + trimmed > width {
            rows.push(Line::from(std::mem::take(&mut row)));
            used = 0;
        }
        if trimmed <= width {
            used += cells(&word);
            row.push(Span::styled(word, style));
            continue;
        }
        let mut piece = String::new();
        for character in word.chars() {
            let cell = cells(&character.to_string());
            if used + cell > width {
                row.push(Span::styled(std::mem::take(&mut piece), style));
                rows.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            piece.push(character);
            used += cell;
        }
        row.push(Span::styled(piece, style));
    }
    rows.push(Line::from(row));
    rows
}

/// A yes-or-no question's buttons, No then Yes, and what Enter answers.
/// The chosen one is marked with arrows as well as its colour, so it shows
/// in every theme and without colour; the other has no background, so the
/// two can't be taken for each other.
pub fn buttons(theme: &Theme, width: u16, yes: bool) -> Line<'static> {
    let (left, right) = if theme.icons.unicode {
        ("▶", "◀")
    } else {
        (">", "<")
    };
    let button = |label: &str, chosen: bool| {
        if chosen {
            Span::styled(format!(" {left} {label} {right} "), theme.selected())
        } else {
            Span::styled(format!("   {label}   "), theme.text())
        }
    };
    let chosen = if yes { "Yes" } else { "No" };
    let mut spans = vec![
        Span::styled("  ", theme.raised()),
        button("No (n)", !yes),
        Span::styled("   ", theme.raised()),
        button("Yes (y)", yes),
    ];
    let hint = format!("   Enter answers {chosen}; arrows change it");
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    if used + hint.chars().count() <= usize::from(width) {
        spans.push(Span::styled(hint, theme.dim()));
    }
    Line::from(spans)
}

/// A box in the middle of `area`, at most `width` by `height`.
pub fn centred(area: Rect, width: u16, height: u16) -> Rect {
    use ratatui::layout::{Constraint, Flex, Layout};
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
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

    #[test]
    fn lines_wrap_at_words_and_long_words_are_cut() {
        let rows = wrap(Line::from("abcdef"), 4);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].to_string(), "ef");
        let rows = wrap(Line::from("one two three"), 8);
        assert_eq!(
            rows.iter().map(|row| row.to_string()).collect::<Vec<_>>(),
            ["one two ", "three"]
        );
    }

    #[test]
    fn the_chosen_button_is_marked_and_enter_says_what_it_answers() {
        let theme = Theme::new(Palette::Midnight, Depth::None, Icons { unicode: true });
        let no = buttons(&theme, 80, false).to_string();
        assert!(no.contains("▶ No (n) ◀"), "{no}");
        assert!(no.contains("   Yes (y)   "), "{no}");
        assert!(no.contains("Enter answers No"), "{no}");
        let yes = buttons(&theme, 80, true).to_string();
        assert!(
            yes.contains("▶ Yes (y) ◀") && yes.contains("Enter answers Yes"),
            "{yes}"
        );
        // Narrow, the buttons stay and the hint goes.
        let narrow = buttons(&theme, 30, false).to_string();
        assert!(
            narrow.contains("▶ No (n) ◀") && !narrow.contains("Enter"),
            "{narrow}"
        );
    }
}
