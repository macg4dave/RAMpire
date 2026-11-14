//! Colour themes and icon sets. A screen names what something means (a
//! [`Tone`], an [`Icon`]); the theme gives it a colour and the icon set a
//! symbol, so nothing is shown by colour alone and every screen works in
//! 256 colours, 16 colours, no colour, Unicode and plain ASCII.

use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;

use crate::tone::Tone;

/// The colours the terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    Colours256,
    Colours16,
    /// `NO_COLOR` or `TERM=dumb`: emphasis only.
    None,
}

impl Depth {
    /// From the environment the TUI runs in: `NO_COLOR` set or
    /// `TERM=dumb` is none; a `256color` terminal or `COLORTERM` is 256;
    /// any other is 16.
    pub fn detect(term: Option<&str>, colorterm: Option<&str>, no_color: bool) -> Self {
        let term = term.unwrap_or("");
        if no_color || term == "dumb" || term.is_empty() {
            Self::None
        } else if term.contains("256color")
            || term.contains("direct")
            || colorterm.is_some_and(|value| !value.is_empty())
        {
            Self::Colours256
        } else {
            Self::Colours16
        }
    }

    pub fn from_environment() -> Self {
        let var = |name: &str| std::env::var(name).ok();
        Self::detect(
            var("TERM").as_deref(),
            var("COLORTERM").as_deref(),
            std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        )
    }
}

/// The palettes a person can choose between with `t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Palette {
    /// The default: light text on navy.
    Navy,
    /// Dark text on a light background.
    Light,
    /// The terminal's own 16 colours on its own background, so it matches
    /// a person's terminal theme.
    Terminal,
}

impl Palette {
    pub const ALL: [Self; 3] = [Self::Navy, Self::Light, Self::Terminal];

    pub fn name(self) -> &'static str {
        match self {
            Self::Navy => "navy",
            Self::Light => "light",
            Self::Terminal => "terminal",
        }
    }

    pub fn next(self) -> Self {
        let at = Self::ALL
            .iter()
            .position(|palette| *palette == self)
            .unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

/// Every colour role a screen uses.
#[derive(Debug, Clone, Copy)]
struct Colours {
    background: Color,
    text: Color,
    value: Color,
    dim: Color,
    frame: Color,
    title: Color,
    bar: Color,
    bar_text: Color,
    track: Color,
    good: Color,
    warn: Color,
    bad: Color,
    info: Color,
    accent: Color,
    selected: Color,
}

const NAVY: Colours = Colours {
    background: Color::Indexed(17),
    text: Color::Indexed(250),
    value: Color::Indexed(231),
    dim: Color::Indexed(67),
    frame: Color::Indexed(45),
    title: Color::Indexed(231),
    bar: Color::Indexed(25),
    bar_text: Color::Indexed(231),
    track: Color::Indexed(24),
    good: Color::Indexed(46),
    warn: Color::Indexed(226),
    bad: Color::Indexed(196),
    info: Color::Indexed(51),
    accent: Color::Indexed(213),
    selected: Color::Indexed(24),
};

const LIGHT: Colours = Colours {
    background: Color::Indexed(255),
    text: Color::Indexed(238),
    value: Color::Indexed(232),
    dim: Color::Indexed(244),
    frame: Color::Indexed(31),
    title: Color::Indexed(232),
    bar: Color::Indexed(31),
    bar_text: Color::Indexed(255),
    track: Color::Indexed(252),
    good: Color::Indexed(28),
    warn: Color::Indexed(130),
    bad: Color::Indexed(160),
    info: Color::Indexed(25),
    accent: Color::Indexed(90),
    selected: Color::Indexed(153),
};

/// The 16 named colours on the terminal's own background.
const TERMINAL: Colours = Colours {
    background: Color::Reset,
    text: Color::Reset,
    value: Color::Reset,
    dim: Color::DarkGray,
    frame: Color::Cyan,
    title: Color::Reset,
    bar: Color::Blue,
    bar_text: Color::White,
    track: Color::DarkGray,
    good: Color::Green,
    warn: Color::Yellow,
    bad: Color::Red,
    info: Color::Cyan,
    accent: Color::Magenta,
    selected: Color::Blue,
};

/// The look in force: a palette at the terminal's depth, and an icon set.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub palette: Palette,
    pub depth: Depth,
    pub icons: Icons,
}

impl Theme {
    pub fn new(palette: Palette, depth: Depth, icons: Icons) -> Self {
        Self {
            palette,
            depth,
            icons,
        }
    }

    fn colours(&self) -> Option<Colours> {
        match (self.depth, self.palette) {
            (Depth::None, _) => None,
            // 16 colours have no navy or light shades: the named colours.
            (Depth::Colours16, _) | (_, Palette::Terminal) => Some(TERMINAL),
            (Depth::Colours256, Palette::Navy) => Some(NAVY),
            (Depth::Colours256, Palette::Light) => Some(LIGHT),
        }
    }

    fn style(&self, pick: impl Fn(&Colours) -> Color) -> Style {
        match self.colours() {
            None => Style::new(),
            Some(colours) => Style::new().fg(pick(&colours)).bg(colours.background),
        }
    }

    /// The screen's own background and text.
    pub fn base(&self) -> Style {
        self.style(|colours| colours.text)
    }

    pub fn text(&self) -> Style {
        self.base()
    }

    pub fn value(&self) -> Style {
        self.style(|colours| colours.value)
    }

    pub fn dim(&self) -> Style {
        match self.colours() {
            None => Style::new(),
            Some(_) => self.style(|colours| colours.dim),
        }
    }

    pub fn frame(&self) -> Style {
        self.style(|colours| colours.frame)
    }

    pub fn title(&self) -> Style {
        self.style(|colours| colours.title)
            .add_modifier(Modifier::BOLD)
    }

    /// The header and status bars.
    pub fn bar(&self) -> Style {
        match self.colours() {
            None => Style::new().add_modifier(Modifier::REVERSED),
            Some(colours) => Style::new().fg(colours.bar_text).bg(colours.bar),
        }
    }

    /// The empty part of a gauge.
    pub fn track(&self) -> Style {
        self.style(|colours| colours.track)
    }

    /// A selected row or tab.
    pub fn selected(&self) -> Style {
        match self.colours() {
            None => Style::new().add_modifier(Modifier::REVERSED),
            Some(colours) => Style::new()
                .fg(colours.value)
                .bg(colours.selected)
                .add_modifier(Modifier::BOLD),
        }
    }

    pub fn tone(&self, tone: Tone) -> Style {
        let style = match tone {
            Tone::Normal => self.text(),
            Tone::Value => self.value(),
            Tone::Dim => self.dim(),
            Tone::Good => self.style(|colours| colours.good),
            Tone::Warn => self.style(|colours| colours.warn),
            Tone::Bad => self.style(|colours| colours.bad),
            Tone::Info => self.style(|colours| colours.info),
            Tone::Accent => self.style(|colours| colours.accent),
        };
        match (self.depth, tone) {
            // Without colour a problem still stands out.
            (Depth::None, Tone::Bad | Tone::Warn) => style.add_modifier(Modifier::BOLD),
            _ => style,
        }
    }

    /// A colour for each VM in the pool's stacked bar and history, in
    /// host-list order, repeating after eight. Without colour every VM is
    /// the same, and the legend names them.
    pub fn series(&self, index: usize) -> Style {
        const SHADES_256: [u8; 8] = [51, 213, 214, 120, 117, 228, 177, 210];
        const SHADES_LIGHT: [u8; 8] = [25, 90, 130, 28, 31, 136, 97, 160];
        const SHADES_16: [Color; 6] = [
            Color::Cyan,
            Color::Magenta,
            Color::Yellow,
            Color::Green,
            Color::Blue,
            Color::Red,
        ];
        let colour = match (self.depth, self.palette) {
            (Depth::None, _) => return Style::new(),
            (Depth::Colours16, _) | (_, Palette::Terminal) => SHADES_16[index % SHADES_16.len()],
            (Depth::Colours256, Palette::Navy) => {
                Color::Indexed(SHADES_256[index % SHADES_256.len()])
            }
            (Depth::Colours256, Palette::Light) => {
                Color::Indexed(SHADES_LIGHT[index % SHADES_LIGHT.len()])
            }
        };
        self.base().fg(colour)
    }

    pub fn border_set(&self) -> symbols::border::Set<'static> {
        if self.icons.unicode {
            symbols::border::ROUNDED
        } else {
            ASCII_BORDER
        }
    }

    pub fn icon(&self, icon: Icon) -> &'static str {
        self.icons.get(icon)
    }
}

const ASCII_BORDER: symbols::border::Set<'static> = symbols::border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Everything a screen shows as a symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Ok,
    Problem,
    Running,
    Stopped,
    Settings,
    Device,
    Vm,
    Growing,
    Shrinking,
    Steady,
    Latched,
    Parked,
    Host,
    Pool,
    Guest,
    Pointer,
    /// A filled cell of a bar.
    Full,
    /// An empty cell of a bar.
    Empty,
}

/// Each meaning with its Unicode and its ASCII symbol: the one table.
pub const ICONS: [(Icon, &str, &str); 18] = [
    (Icon::Ok, "✓", "[ok]"),
    (Icon::Problem, "⚠", "[!!]"),
    (Icon::Running, "●", "*"),
    (Icon::Stopped, "○", "o"),
    (Icon::Settings, "⚙", "[=]"),
    (Icon::Device, "▣", "[#]"),
    (Icon::Vm, "◆", "<>"),
    (Icon::Growing, "▲", "^"),
    (Icon::Shrinking, "▼", "v"),
    (Icon::Steady, "■", "="),
    (Icon::Latched, "✖", "[X]"),
    (Icon::Parked, "◇", "<.>"),
    (Icon::Host, "⌂", "[h]"),
    (Icon::Pool, "≋", "[p]"),
    (Icon::Guest, "◎", "(g)"),
    (Icon::Pointer, "▶", ">"),
    (Icon::Full, "█", "#"),
    (Icon::Empty, "░", "."),
];

/// Unicode symbols, or plain ASCII for a terminal or locale without UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icons {
    pub unicode: bool,
}

impl Icons {
    /// Unicode when the locale is UTF-8 and the terminal isn't dumb.
    pub fn detect(locale: Option<&str>, term: Option<&str>) -> Self {
        let utf8 = locale.is_some_and(|locale| {
            let locale = locale.to_ascii_lowercase();
            locale.contains("utf-8") || locale.contains("utf8")
        });
        Self {
            unicode: utf8 && term.is_some_and(|term| term != "dumb"),
        }
    }

    pub fn from_environment() -> Self {
        // The first of these that is set decides, as the C library reads them.
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()));
        Self::detect(locale.as_deref(), std::env::var("TERM").ok().as_deref())
    }

    pub fn get(self, icon: Icon) -> &'static str {
        ICONS
            .iter()
            .find(|(meaning, _, _)| *meaning == icon)
            .map_or(
                "?",
                |(_, unicode, ascii)| {
                    if self.unicode {
                        unicode
                    } else {
                        ascii
                    }
                },
            )
    }

    /// The eight-level bar set for sparklines.
    pub fn bars(self) -> symbols::bar::Set<'static> {
        if self.unicode {
            symbols::bar::NINE_LEVELS
        } else {
            symbols::bar::Set {
                full: "#",
                seven_eighths: "#",
                three_quarters: "=",
                five_eighths: "=",
                half: "-",
                three_eighths: "-",
                one_quarter: ".",
                one_eighth: ".",
                empty: " ",
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_follows_the_terminal() {
        assert_eq!(
            Depth::detect(Some("xterm-256color"), None, false),
            Depth::Colours256
        );
        assert_eq!(
            Depth::detect(Some("screen"), Some("truecolor"), false),
            Depth::Colours256
        );
        assert_eq!(Depth::detect(Some("xterm"), None, false), Depth::Colours16);
        assert_eq!(Depth::detect(Some("vt100"), None, false), Depth::Colours16);
        assert_eq!(Depth::detect(Some("dumb"), None, false), Depth::None);
        assert_eq!(
            Depth::detect(Some("xterm-256color"), None, true),
            Depth::None
        );
        assert_eq!(Depth::detect(None, None, false), Depth::None);
    }

    #[test]
    fn icons_are_ascii_without_a_utf8_locale() {
        assert!(Icons::detect(Some("en_GB.UTF-8"), Some("xterm")).unicode);
        assert!(Icons::detect(Some("C.utf8"), Some("screen")).unicode);
        assert!(!Icons::detect(Some("C"), Some("xterm")).unicode);
        assert!(!Icons::detect(None, Some("xterm")).unicode);
        assert!(!Icons::detect(Some("en_US.UTF-8"), Some("dumb")).unicode);
    }

    #[test]
    fn every_icon_has_a_unicode_and_an_ascii_symbol() {
        for (icon, unicode, ascii) in ICONS {
            assert!(!unicode.is_empty() && !ascii.is_empty(), "{icon:?}");
            assert!(ascii.is_ascii(), "{icon:?}");
            assert_eq!(Icons { unicode: true }.get(icon), unicode);
            assert_eq!(Icons { unicode: false }.get(icon), ascii);
        }
    }

    #[test]
    fn without_colour_problems_are_bold_and_selection_reversed() {
        let theme = Theme::new(Palette::Navy, Depth::None, Icons { unicode: false });
        assert!(theme.tone(Tone::Bad).add_modifier.contains(Modifier::BOLD));
        assert!(theme.selected().add_modifier.contains(Modifier::REVERSED));
        assert_eq!(theme.tone(Tone::Good).fg, None);
        let theme = Theme::new(Palette::Navy, Depth::Colours256, Icons { unicode: true });
        assert_eq!(theme.base().bg, Some(Color::Indexed(17)));
        let sixteen = Theme::new(Palette::Navy, Depth::Colours16, Icons { unicode: true });
        assert_eq!(sixteen.tone(Tone::Bad).fg, Some(Color::Red));
    }
}
